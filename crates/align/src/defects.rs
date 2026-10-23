//! Sensor defects: dust on the sensor and hot pixels, which sit at the same sensor pixels in every frame, found and
//! repaired in each frame before alignment warps it. Once a frame is warped, a defect moves with the frame's
//! transform, so that stacking smears it into a trail of copies; repaired at the sensor's pixels, it leaves none.
//!
//! - **The mask** ([`DefectMask`]): one flag per sensor pixel, in the frames' own pixel grid. The user can supply one
//!   ([`DefectMask::from_image`]: black spots on white, the frames' size), and a detected mask can be saved as such an
//!   image ([`DefectMask::to_image`]), edited in any image editor and loaded back.
//! - **Detection** ([`DefectDetector`], [`detect_defects`]): dust is darker than its surroundings on every side, by
//!   the same factor in every frame, at the same sensor coordinates, and a hot pixel brighter than its neighbors in
//!   most frames. The scene moves across the sensor from frame to frame (focus breathing, a rail, vibration), and its
//!   detail sharpens and blurs as the focus passes it, so it rarely stays darker by a steady factor at one pixel; a
//!   defect always does. See [`DefectDetector`] for the tests and their thresholds.
//! - **Inpainting** ([`inpaint`]): each spot of the mask is filled from the pixels around it, by pull-push
//!   interpolation (S. J. Gortler, R. Grzeszczuk, R. Szeliski and M. F. Cohen, "The Lumigraph", SIGGRAPH 1996): a
//!   pyramid of weighted averages of the known pixels, pulled down to where the spot is covered, then pushed back up,
//!   which fills a hole smoothly from its whole border. It runs before a frame is aligned, so that alignment doesn't
//!   see the defects either, and before it is warped for stacking.
//!
//! A defect's pixels lose the scene's detail behind them: the fill is smooth. That costs little where dust shows most,
//! on a defocused backdrop; on the subject, the stack loses the detail under the mask, which is why detected spots
//! are kept to what is dust (see [`DefectDetector`] for what can still pass for it, and what is found only in part).
//! Review a detected mask before stacking with it.

use florastack_core::convert::srgb_to_linear;
use florastack_core::ops::{BlurMethod, gaussian_blur_in_place};
use florastack_core::{ColorSpace, Encoding, Image, Monitor};
use rayon::prelude::*;

use crate::error::AlignError;
use crate::prepare::{MIN_SIDE, luminance};

/// The most frames [`DefectDetector`] counts: its counters are bytes. [`detection_frames`] picks fewer.
pub const MAX_DETECTION_FRAMES: usize = 255;
/// The most spots a mask may have: real dust makes a few hundred at most, and a mask of millions of single pixels, such
/// as a grid of dots, would take gigabytes to hold.
pub const MAX_SPOTS: usize = 100_000;
/// The fewest frames detection needs: with fewer, noise could pass for a defect.
pub const MIN_DETECTION_FRAMES: usize = 5;
/// How many frames of a stack [`detection_frames`] picks at most: a sample that tells "most frames" well enough.
const SAMPLE_FRAMES: usize = 32;
/// A hot pixel is brighter than its neighbors in at least this share of the frames.
const MOST: f64 = 0.8;
/// Dust can't be told where the surroundings are darker than this (linear, 1 = white): noise hides it there.
const DARK_FLOOR: f32 = 0.01;
/// The ring that dust is compared with: at least this radius, in pixels, and this share of the frame's longer side.
const MIN_RING: f64 = 12.0;
const RING_SHARE: f64 = 0.005;
/// The number of samples on the dust ring.
const RING_SAMPLES: usize = 16;
/// A hot pixel: its brightest channel exceeds the median of its neighbors two pixels away by this factor of that
/// median plus this much (linear).
const HOT: f32 = 0.5;
const HOT_FLOOR: f32 = 0.02;
/// Detected dust grows by its radius, at least by this many pixels of the dust plane, to cover the faint rim of its
/// shadow; hot pixels by one, to cover what demosaicing spread to their neighbors.
const DUST_GROWTH: usize = 3;
const HOT_GROWTH: usize = 1;
/// The fill of a spot uses the pixels within this many pixels of it: its border, rather than what lies beyond, such
/// as the edge of a dark object nearby.
const FILL_MARGIN: usize = 4;

/// An 8-connected group of masked pixels: its bounding box and its area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spot {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
    /// How many pixels it has.
    pub area: usize,
}

/// Which sensor pixels are defective. See the module documentation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefectMask {
    width: usize,
    height: usize,
    /// One flag per pixel, row by row.
    masked: Vec<bool>,
    /// The spots, in the order of their first pixel, row by row, each with its pixels' indices in that order.
    spots: Vec<(Spot, Vec<usize>)>,
}

impl DefectMask {
    /// A `width` x `height` mask with every pixel for which `masked` holds.
    pub fn from_fn(width: usize, height: usize, masked: impl Fn(usize, usize) -> bool + Sync) -> Result<DefectMask, AlignError> {
        check_area(width, height)?;
        let mut flags = vec![false; width * height];
        flags.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
            for (x, m) in row.iter_mut().enumerate() {
                *m = masked(x, y);
            }
        });
        DefectMask::from_flags(width, height, flags)
    }

    /// A mask with nothing masked.
    pub fn empty(width: usize, height: usize) -> Result<DefectMask, AlignError> {
        DefectMask::from_fn(width, height, |_, _| false)
    }

    /// The mask an image draws: black spots on white. A pixel is masked when the mean of its color channels (alpha
    /// left out) is below one half, as stored (so `0` to `127` of 255 in an 8-bit image); a pixel that isn't a number
    /// is masked too.
    pub fn from_image(image: &Image<f32>) -> Result<DefectMask, AlignError> {
        let colors = match image.channels() {
            2 => 1,
            4 => 3,
            c => c,
        };
        let (w, h, c) = (image.width(), image.height(), image.channels());
        let mut flags = vec![false; w * h];
        flags.par_chunks_mut(w).zip(image.par_rows()).for_each(|(flags, row)| {
            for (m, pixel) in flags.iter_mut().zip(row.chunks_exact(c)) {
                let mean = pixel.iter().take(colors).sum::<f32>() / colors as f32;
                *m = mean.is_nan() || mean < 0.5;
            }
        });
        DefectMask::from_flags(w, h, flags)
    }

    fn from_flags(width: usize, height: usize, masked: Vec<bool>) -> Result<DefectMask, AlignError> {
        let spots = label(&masked, width, height)?;
        Ok(DefectMask { width, height, masked, spots })
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    /// Whether pixel `(x, y)` is masked; pixels outside the mask aren't.
    pub fn is_masked(&self, x: usize, y: usize) -> bool {
        x < self.width && self.masked.get(y * self.width + x).copied().unwrap_or(false)
    }

    /// How many pixels are masked.
    pub fn count(&self) -> usize {
        self.spots.iter().map(|(s, _)| s.area).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.spots.is_empty()
    }

    /// The spots: the mask's 8-connected groups of pixels, in the order of their first pixel, row by row.
    pub fn spots(&self) -> impl ExactSizeIterator<Item = Spot> + '_ {
        self.spots.iter().map(|(s, _)| *s)
    }

    /// The mask as an image to save and edit: one channel, black (0) where masked on white (255).
    pub fn to_image(&self) -> Result<Image<u8>, AlignError> {
        let mut image = Image::new(self.width, self.height, 1, ColorSpace::SRGB)?;
        image.par_rows_mut().zip(self.masked.par_chunks(self.width)).for_each(|(row, flags)| {
            for (v, &m) in row.iter_mut().zip(flags) {
                *v = if m { 0 } else { 255 };
            }
        });
        Ok(image)
    }

    /// Checks that the mask fits frames of `width` x `height` pixels.
    pub fn check_size(&self, width: usize, height: usize) -> Result<(), AlignError> {
        if (self.width, self.height) == (width, height) {
            Ok(())
        } else {
            Err(AlignError::MaskSize { width: self.width, height: self.height, frame_width: width, frame_height: height })
        }
    }

    /// The mask with every masked pixel grown by `radius` pixels, within a disc.
    pub fn grown(&self, radius: usize) -> Result<DefectMask, AlignError> {
        let flags = grow(&self.masked, self.width, self.height, self.spots.iter().flat_map(|(_, p)| p.iter().copied()), radius);
        DefectMask::from_flags(self.width, self.height, flags)
    }
}

/// Checks a mask's or a detector's size.
fn check_area(width: usize, height: usize) -> Result<(), AlignError> {
    if width == 0 || height == 0 || width.checked_mul(height).is_none_or(|n| isize::try_from(n).is_err()) {
        return Err(AlignError::MaskSize { width, height, frame_width: width, frame_height: height });
    }
    Ok(())
}

/// The 8-connected groups of `masked` pixels, in the order of their first pixel, with their pixels in raster order; an
/// error past [`MAX_SPOTS`] of them.
fn label(masked: &[bool], width: usize, height: usize) -> Result<Vec<(Spot, Vec<usize>)>, AlignError> {
    let mut seen = vec![false; masked.len()];
    let mut spots = Vec::new();
    let mut stack = Vec::new();
    for start in 0..masked.len() {
        if !masked.get(start).copied().unwrap_or(false) || seen.get(start).copied().unwrap_or(true) {
            continue;
        }
        let mut pixels = Vec::new();
        stack.push(start);
        if let Some(s) = seen.get_mut(start) {
            *s = true;
        }
        while let Some(i) = stack.pop() {
            pixels.push(i);
            let (x, y) = (i % width, i / width);
            for ny in y.saturating_sub(1)..=(y + 1).min(height - 1) {
                for nx in x.saturating_sub(1)..=(x + 1).min(width - 1) {
                    let n = ny * width + nx;
                    if masked.get(n).copied().unwrap_or(false) && !seen.get(n).copied().unwrap_or(true) {
                        if let Some(s) = seen.get_mut(n) {
                            *s = true;
                        }
                        stack.push(n);
                    }
                }
            }
        }
        pixels.sort_unstable();
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        for &i in &pixels {
            let (x, y) = (i % width, i / width);
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
        let spot = Spot { x: x0, y: y0, width: x1 - x0 + 1, height: y1 - y0 + 1, area: pixels.len() };
        if spots.len() == MAX_SPOTS {
            return Err(AlignError::MaskSpots { max: MAX_SPOTS });
        }
        spots.push((spot, pixels));
    }
    Ok(spots)
}

/// `masked` with the pixels within `radius` of each of `seeds` set too.
fn grow(masked: &[bool], width: usize, height: usize, seeds: impl Iterator<Item = usize>, radius: usize) -> Vec<bool> {
    let mut out = masked.to_vec();
    grow_into(&mut out, width, height, seeds, radius);
    out
}

/// Sets the pixels of `out` within `radius` of each of `seeds`.
fn grow_into(out: &mut [bool], width: usize, height: usize, seeds: impl Iterator<Item = usize>, radius: usize) {
    if radius == 0 {
        return;
    }
    let r2 = radius * radius;
    let offsets: Vec<(isize, isize)> = (-signed(radius)..=signed(radius))
        .flat_map(|dy| (-signed(radius)..=signed(radius)).map(move |dx| (dx, dy)))
        .filter(|&(dx, dy)| dx.unsigned_abs().pow(2) + dy.unsigned_abs().pow(2) <= r2)
        .collect();
    for i in seeds {
        let (x, y) = (i % width, i / width);
        for &(dx, dy) in &offsets {
            if let (Some(nx), Some(ny)) = (x.checked_add_signed(dx), y.checked_add_signed(dy))
                && nx < width
                && ny < height
                && let Some(m) = out.get_mut(ny * width + nx)
            {
                *m = true;
            }
        }
    }
}

/// A small radius as a signed offset.
fn signed(v: usize) -> isize {
    isize::try_from(v).unwrap_or(isize::MAX)
}

/// Fills the masked pixels of `frame` from the pixels around them (see the module documentation). Every channel is
/// filled; pixels that aren't masked are left exactly as they are, and so is a spot with no known pixel near it.
pub fn inpaint(frame: &mut Image<f32>, mask: &DefectMask) -> Result<(), AlignError> {
    mask.check_size(frame.width(), frame.height())?;
    let c = frame.channels();
    let fills: Vec<Option<Vec<f32>>> = mask.spots.par_iter().map(|(spot, pixels)| fill_spot(frame, mask, spot, pixels)).collect();
    let w = frame.width();
    for ((_, pixels), fill) in mask.spots.iter().zip(fills) {
        let Some(fill) = fill else { continue };
        for (&i, values) in pixels.iter().zip(fill.chunks_exact(c)) {
            if let Some(pixel) = frame.pixel_mut(i % w, i / w) {
                pixel.copy_from_slice(values);
            }
        }
    }
    Ok(())
}

/// The values of a spot's pixels, in its order, filled by pull-push from the known pixels of a window around it, or
/// `None` when the window has no known pixel.
fn fill_spot(frame: &Image<f32>, mask: &DefectMask, spot: &Spot, pixels: &[usize]) -> Option<Vec<f32>> {
    let (fw, fh, c) = (frame.width(), frame.height(), frame.channels());
    let margin = FILL_MARGIN;
    let x0 = spot.x.saturating_sub(margin);
    let y0 = spot.y.saturating_sub(margin);
    let x1 = (spot.x + spot.width + margin).min(fw);
    let y1 = (spot.y + spot.height + margin).min(fh);
    let (w, h) = (x1 - x0, y1 - y0);
    let mut values = vec![0.0_f32; w * h * c];
    let mut weights = vec![0.0_f32; w * h];
    let mut known = false;
    for y in 0..h {
        let Some(row) = frame.row(y0 + y) else { continue };
        for x in 0..w {
            let Some(pixel) = row.get((x0 + x) * c..(x0 + x + 1) * c) else { continue };
            if mask.is_masked(x0 + x, y0 + y) || !pixel.iter().all(|v| v.is_finite()) {
                continue;
            }
            if let (Some(dst), Some(weight)) = (values.get_mut((y * w + x) * c..(y * w + x + 1) * c), weights.get_mut(y * w + x)) {
                dst.copy_from_slice(pixel);
                *weight = 1.0;
                known = true;
            }
        }
    }
    if !known {
        return None;
    }
    pull_push(&mut values, &weights, w, h, c);
    let mut out = Vec::with_capacity(pixels.len() * c);
    for &i in pixels {
        let (x, y) = ((i % fw).checked_sub(x0)?, (i / fw).checked_sub(y0)?);
        out.extend_from_slice(values.get((y * w + x) * c..(y * w + x + 1) * c)?);
    }
    Some(out)
}

/// One level of the pull-push pyramid: `width` x `height` values of `c` channels and their weights, from 0 to 1.
struct Plane {
    width: usize,
    height: usize,
    values: Vec<f32>,
    weights: Vec<f32>,
}

/// Fills the pixels of `values` whose weight is below 1 from the others (see the module documentation).
fn pull_push(values: &mut [f32], weights: &[f32], width: usize, height: usize, c: usize) {
    // Pull: each coarser level averages 2 x 2 pixels of the finer one, weighted, its weight their sum up to 1.
    let mut levels: Vec<Plane> = Vec::new();
    let (mut w, mut h) = (width, height);
    while w > 1 || h > 1 {
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let (fine_values, fine_weights): (&[f32], &[f32]) = match levels.last() {
            Some(p) => (&p.values, &p.weights),
            None => (values, weights),
        };
        let mut coarse = Plane { width: cw, height: ch, values: vec![0.0; cw * ch * c], weights: vec![0.0; cw * ch] };
        for y in 0..ch {
            for x in 0..cw {
                let mut sum = [0.0_f32; 4];
                let mut total = 0.0_f32;
                for (fx, fy) in [(2 * x, 2 * y), (2 * x + 1, 2 * y), (2 * x, 2 * y + 1), (2 * x + 1, 2 * y + 1)] {
                    if fx >= w || fy >= h {
                        continue;
                    }
                    let i = fy * w + fx;
                    let wt = fine_weights.get(i).copied().unwrap_or(0.0);
                    if wt <= 0.0 {
                        continue;
                    }
                    total += wt;
                    for (s, &v) in sum.iter_mut().zip(fine_values.get(i * c..i * c + c).unwrap_or(&[])) {
                        *s += wt * v;
                    }
                }
                if total > 0.0 {
                    let i = y * cw + x;
                    if let Some(dst) = coarse.values.get_mut(i * c..i * c + c) {
                        for (d, s) in dst.iter_mut().zip(sum) {
                            *d = s / total;
                        }
                    }
                    if let Some(wt) = coarse.weights.get_mut(i) {
                        *wt = total.min(1.0);
                    }
                }
            }
        }
        levels.push(coarse);
        (w, h) = (cw, ch);
    }
    // Push: from the coarsest level down, each pixel takes the coarser level's bilinear value for what its own
    // weight lacks.
    let mut top_weights = weights.to_vec();
    for k in (0..levels.len()).rev() {
        let (finer, coarser) = levels.split_at_mut(k);
        let Some(coarse) = coarser.first() else { continue };
        let (fine_values, fine_weights, fw, fh) = match finer.last_mut() {
            Some(f) => (&mut f.values[..], &mut f.weights[..], f.width, f.height),
            None => (&mut *values, &mut top_weights[..], width, height),
        };
        for y in 0..fh {
            for x in 0..fw {
                let i = y * fw + x;
                let wt = fine_weights.get(i).copied().unwrap_or(0.0);
                if wt >= 1.0 {
                    continue;
                }
                let up = bilinear(&coarse.values, coarse.width, coarse.height, c, x as f32 * 0.5 - 0.25, y as f32 * 0.5 - 0.25);
                if let Some(dst) = fine_values.get_mut(i * c..i * c + c) {
                    for (d, u) in dst.iter_mut().zip(up) {
                        *d = wt * *d + (1.0 - wt) * u;
                    }
                }
                if let Some(w) = fine_weights.get_mut(i) {
                    *w = 1.0;
                }
            }
        }
    }
}

/// `values` (`width` x `height` pixels of `c` channels, at most 4) at `(x, y)`, bilinearly, clamped to the edges.
fn bilinear(values: &[f32], width: usize, height: usize, c: usize, x: f32, y: f32) -> [f32; 4] {
    let x = x.clamp(0.0, (width - 1) as f32);
    let y = y.clamp(0.0, (height - 1) as f32);
    let (x0, y0) = (to_index(x), to_index(y));
    let (x1, y1) = ((x0 + 1).min(width - 1), (y0 + 1).min(height - 1));
    let (tx, ty) = (x - x0 as f32, y - y0 as f32);
    let at = |x: usize, y: usize, k: usize| values.get((y * width + x) * c + k).copied().unwrap_or(0.0);
    let mut out = [0.0; 4];
    for (k, o) in out.iter_mut().enumerate().take(c) {
        let top = at(x0, y0, k) + tx * (at(x1, y0, k) - at(x0, y0, k));
        let bottom = at(x0, y1, k) + tx * (at(x1, y1, k) - at(x0, y1, k));
        *o = top + ty * (bottom - top);
    }
    out
}

/// A non-negative, finite coordinate's whole part.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Clamped to the plane before.
fn to_index(v: f32) -> usize {
    v.max(0.0) as usize
}

/// Which frames of a stack of `frames` to detect defects on: every frame of a short stack, at most 32 spread evenly
/// over a longer one, both ends included.
pub fn detection_frames(frames: usize) -> Vec<usize> {
    if frames <= SAMPLE_FRAMES {
        return (0..frames).collect();
    }
    let mut picked: Vec<usize> = (0..SAMPLE_FRAMES).map(|k| k * (frames - 1) / (SAMPLE_FRAMES - 1)).collect();
    picked.dedup();
    picked
}

/// Finds the defects of a stack's frames, which are added one at a time, at the same sensor coordinates.
///
/// **Dust** is a shadow: darker than its surroundings on every side, by the same factor in every frame, whatever the
/// focus. The scene's own dark details are darker than only part of their surroundings (the rim of a dark object), or
/// change as the focus passes them (texture, sharp in some frames and blurred in others), or move across the sensor
/// with focus breathing. Dust is found on a luminance plane (blocks of 2 x 2 pixels averaged for frames above 4096
/// pixels, which dust is larger than), smoothed by a Gaussian of one pixel:
///
/// 1. In each frame, each pixel's ratio to its surroundings: to the sixth darkest of 16 samples on a ring around it,
///    of 12 pixels' radius or 0.5% of the frame's longer side, whichever is larger. The ring reaches past any dust spot
///    smaller than it, and a pixel must be darker than about two thirds of its surroundings to have a ratio below 1:
///    near the rim of a dark object, the samples inside the object keep the ratio near 1. Where that sample is darker
///    than 1% of white, noise hides dust and the frame doesn't count.
/// 2. Over the frames that count (at least [`MIN_DETECTION_FRAMES`] and half of them), the ratio's mean and spread.
///    The cores of dust darken by at least 8% on average, with a spread of at most 12% of the darkening: groups of at
///    least four such pixels. Each core then takes the pixels within three pixels of it that darken by at least 3%
///    with a spread of at most 40% of that (a hysteresis, as edges are followed in J. Canny, "A computational approach
///    to edge detection", IEEE PAMI 8(6), 1986).
/// 3. Each spot is made symmetric about its center (its pixels weighted by their darkening), as a shadow is: next to a
///    dark object, the ring samples in the object hide the spot's darkening on that side. It then grows by its own
///    radius (at least three pixels) to cover the faint rim of its shadow, down to about 1% of darkening, which the
///    ring, inside that rim on a large spot, measures short.
///
/// **Hot pixels**, at full resolution: the pixel's brightest channel is more than 1.5 times the median of the same
/// channel's maximum at its eight neighbors two pixels away, plus 2% of white, in at least 80% of the frames; each
/// grows by one pixel, for what demosaicing spread around it. Noise is brighter in fewer than half the frames.
///
/// What still passes for dust is a detail of the scene that stays at the same sensor pixels, darker than all around it
/// by the same factor in every frame: dense, coarse texture that every frame shows nearly in focus, on a stack that
/// neither breathes nor moves (on the synthetic test scenes' textured background, about a third of the frame). Dust
/// that straddles the edge of a dark object is found only in part: over the object, noise hides how steady its shadow
/// is, and beside it, the ring samples in the object hide its darkening.
///
/// The detector holds ten bytes per pixel of the dust plane and one per pixel of the frame, whatever the number of
/// frames: it never keeps a frame.
pub struct DefectDetector {
    width: usize,
    height: usize,
    frames: usize,
    /// The dust plane: blocks of `block` x `block` pixels, `plane_width` x `plane_height` of them.
    block: usize,
    plane_width: usize,
    plane_height: usize,
    ring: Vec<(isize, isize)>,
    /// Per dust-plane pixel: how many frames counted, and the sums of the ratio and its square over them.
    counted: Vec<u8>,
    sum: Vec<f32>,
    squares: Vec<f32>,
    /// Per frame pixel: in how many frames it was brighter than its neighbors.
    bright: Vec<u8>,
}

/// Frames above this many pixels on their longer side are looked for dust at half resolution.
const DUST_SIDE: usize = 4096;
/// The ring sample a pixel is compared with, from the darkest (0) of [`RING_SAMPLES`].
const RING_RANK: usize = 5;
/// The luminance is smoothed by a Gaussian of this many pixels of the dust plane.
const DUST_SMOOTHING: f64 = 1.0;
/// Dust cores: at least this mean darkening, a spread of at most this share of it, and groups of at least this many
/// pixels.
const CORE_DARK: f32 = 0.08;
const CORE_SPREAD: f32 = 0.12;
const CORE_AREA: usize = 4;
/// The pixels a core takes: within this many pixels of it, at least this darkening, at most this spread.
const REACH: usize = 3;
const RIM_DARK: f32 = 0.03;
const RIM_SPREAD: f32 = 0.4;

impl DefectDetector {
    /// A detector for frames of `width` x `height` pixels.
    pub fn new(width: usize, height: usize) -> Result<DefectDetector, AlignError> {
        if width < MIN_SIDE || height < MIN_SIDE {
            return Err(AlignError::TooSmall { width, height });
        }
        check_area(width, height)?;
        let long = width.max(height);
        let block = if long > DUST_SIDE { 2 } else { 1 };
        let (plane_width, plane_height) = (width / block, height / block);
        let radius = MIN_RING.max(RING_SHARE * long as f64) / block as f64;
        let ring = (0..RING_SAMPLES)
            .map(|k| {
                let angle = std::f64::consts::TAU * k as f64 / RING_SAMPLES as f64;
                (round(radius * angle.cos()), round(radius * angle.sin()))
            })
            .collect();
        let n = plane_width * plane_height;
        Ok(DefectDetector {
            width,
            height,
            frames: 0,
            block,
            plane_width,
            plane_height,
            ring,
            counted: vec![0; n],
            sum: vec![0.0; n],
            squares: vec![0.0; n],
            bright: vec![0; width * height],
        })
    }

    /// How many frames have been added.
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Counts `frame`, which must have the detector's size. Stops with [`AlignError::Cancelled`] when `monitor` is
    /// cancelled.
    pub fn add(&mut self, frame: &Image<f32>, monitor: &dyn Monitor) -> Result<(), AlignError> {
        if (frame.width(), frame.height()) != (self.width, self.height) {
            return Err(AlignError::FrameSize {
                index: self.frames,
                width: frame.width(),
                height: frame.height(),
                expected_width: self.width,
                expected_height: self.height,
            });
        }
        if self.frames >= MAX_DETECTION_FRAMES {
            return Err(AlignError::DetectionFrames { max: MAX_DETECTION_FRAMES });
        }
        monitor.checkpoint()?;
        self.add_dust(frame)?;
        monitor.checkpoint()?;
        self.add_hot(frame);
        self.frames += 1;
        Ok(())
    }

    fn add_dust(&mut self, frame: &Image<f32>) -> Result<(), AlignError> {
        let mut plane = luminance(frame, self.block)?;
        gaussian_blur_in_place(&mut plane, DUST_SMOOTHING, BlurMethod::Exact)?;
        let (w, h) = (self.plane_width, self.plane_height);
        let luma: Vec<f32> = plane.rows().flat_map(|r| r.iter().take(w)).copied().collect();
        let ring = &self.ring;
        self.counted.par_chunks_mut(w).zip(self.sum.par_chunks_mut(w)).zip(self.squares.par_chunks_mut(w)).enumerate().for_each(
            |(y, ((counted, sum), squares))| {
                let mut samples = [0.0_f32; RING_SAMPLES];
                for x in 0..w {
                    let Some(&l) = luma.get(y * w + x) else { continue };
                    for (s, &(dx, dy)) in samples.iter_mut().zip(ring) {
                        let sx = x.saturating_add_signed(dx).min(w - 1);
                        let sy = y.saturating_add_signed(dy).min(h - 1);
                        *s = luma.get(sy * w + sx).copied().unwrap_or(0.0);
                    }
                    samples.select_nth_unstable_by(RING_RANK, f32::total_cmp);
                    let surroundings = samples.get(RING_RANK).copied().unwrap_or(0.0);
                    if !(surroundings > DARK_FLOOR && l.is_finite()) {
                        continue;
                    }
                    let ratio = l / surroundings;
                    if let (Some(c), Some(s), Some(q)) = (counted.get_mut(x), sum.get_mut(x), squares.get_mut(x)) {
                        *c = c.saturating_add(1);
                        *s += ratio;
                        *q += ratio * ratio;
                    }
                }
            },
        );
        Ok(())
    }

    fn add_hot(&mut self, frame: &Image<f32>) {
        let (w, h) = (self.width, self.height);
        let brightest = brightest(frame);
        let near: [(isize, isize); 8] = [(-2, -2), (0, -2), (2, -2), (-2, 0), (2, 0), (-2, 2), (0, 2), (2, 2)];
        self.bright.par_chunks_mut(w).enumerate().for_each(|(y, bright)| {
            let mut samples = [0.0_f32; 8];
            for (x, b) in bright.iter_mut().enumerate() {
                let Some(&m) = brightest.get(y * w + x) else { continue };
                let mut least = f32::INFINITY;
                for (s, &(dx, dy)) in samples.iter_mut().zip(&near) {
                    let sx = x.saturating_add_signed(dx).min(w - 1);
                    let sy = y.saturating_add_signed(dy).min(h - 1);
                    *s = brightest.get(sy * w + sx).copied().unwrap_or(f32::NAN);
                    least = least.min(*s);
                }
                // The median is at least the darkest neighbor: a pixel no brighter than that can't pass, and most don't.
                let can_pass = m > least * (1.0 + HOT) + HOT_FLOOR;
                if !can_pass {
                    continue;
                }
                if let Some(median) = median(&mut samples)
                    && m > median * (1.0 + HOT) + HOT_FLOOR
                {
                    *b = b.saturating_add(1);
                }
            }
        });
    }

    /// The defect mask of the frames added (see [`DefectDetector`]). Needs at least [`MIN_DETECTION_FRAMES`] frames.
    pub fn finish(self) -> Result<DefectMask, AlignError> {
        if self.frames < MIN_DETECTION_FRAMES {
            return Err(AlignError::FewFrames { frames: self.frames, min: MIN_DETECTION_FRAMES });
        }
        let dust = self.dust()?;
        let n = self.width * self.height;
        let most = (MOST * self.frames as f64).ceil();
        let hot: Vec<bool> = self.bright.iter().map(|&b| f64::from(b) >= most).collect();
        let seeds: Vec<usize> = hot.iter().enumerate().filter(|(_, h)| **h).map(|(i, _)| i).collect();
        let hot = grow(&hot, self.width, self.height, seeds.into_iter(), HOT_GROWTH);
        let k = self.block;
        let (pw, ph) = (self.plane_width, self.plane_height);
        let mut masked = vec![false; n];
        masked.par_chunks_mut(self.width).enumerate().for_each(|(y, row)| {
            let py = (y / k).min(ph.saturating_sub(1));
            for (x, m) in row.iter_mut().enumerate() {
                let px = (x / k).min(pw.saturating_sub(1));
                *m = dust.get(py * pw + px).copied().unwrap_or(false) || hot.get(y * self.width + x).copied().unwrap_or(false);
            }
        });
        DefectMask::from_flags(self.width, self.height, masked)
    }

    /// The dust mask on the dust plane. See [`DefectDetector`].
    fn dust(&self) -> Result<Vec<bool>, AlignError> {
        let (w, h) = (self.plane_width, self.plane_height);
        let min_counted = MIN_DETECTION_FRAMES.max(self.frames.div_ceil(2));
        // The mean darkening and the spread of the ratio, where enough frames counted.
        let stats: Vec<Option<(f32, f32)>> = (0..w * h)
            .into_par_iter()
            .map(|i| {
                let n = usize::from(self.counted.get(i).copied().unwrap_or(0));
                if n < min_counted {
                    return None;
                }
                let n = n as f32;
                let mean = self.sum.get(i)? / n;
                let spread = (self.squares.get(i)? / n - mean * mean).max(0.0).sqrt();
                Some((1.0 - mean, spread))
            })
            .collect();
        let passes =
            |dark: f32, spread: f32| stats.iter().map(move |s| s.is_some_and(|(d, sd)| d >= dark && sd <= spread * d)).collect::<Vec<bool>>();
        let cores = passes(CORE_DARK, CORE_SPREAD);
        let mut kept = vec![false; w * h];
        for (spot, pixels) in label(&cores, w, h)? {
            if spot.area >= CORE_AREA {
                for i in pixels {
                    if let Some(k) = kept.get_mut(i) {
                        *k = true;
                    }
                }
            }
        }
        let seeds = |flags: &[bool]| flags.iter().enumerate().filter(|(_, m)| **m).map(|(i, _)| i).collect::<Vec<usize>>();
        let reach = grow(&kept, w, h, seeds(&kept).into_iter(), REACH);
        let rim = passes(RIM_DARK, RIM_SPREAD);
        let region: Vec<bool> = kept.iter().zip(&reach).zip(&rim).map(|((&k, &r), &m)| k || (r && m)).collect();
        // Each spot is made symmetric about its center, its pixels weighted by their darkening: a shadow is, and next
        // to a dark object, the ring samples in the object hide the spot's darkening on that side. Then it grows by
        // its radius, at least by DUST_GROWTH, since the faint rim of a shadow is wider on a larger spot.
        let mut out = region.clone();
        for (spot, pixels) in label(&region, w, h)? {
            let (mut sx, mut sy, mut total) = (0.0, 0.0, 0.0);
            for &i in &pixels {
                let weight = f64::from(stats.get(i).copied().flatten().map_or(0.0, |(d, _)| d.max(0.0)));
                (sx, sy, total) = (sx + weight * (i % w) as f64, sy + weight * (i / w) as f64, total + weight);
            }
            let mut symmetric = pixels.clone();
            if total > 0.0 {
                let (cx, cy) = (sx / total, sy / total);
                symmetric.extend(pixels.iter().filter_map(|&i| {
                    let (x, y) = (2.0 * cx - (i % w) as f64, 2.0 * cy - (i / w) as f64);
                    let (x, y) = (x.round(), y.round());
                    (x >= 0.0 && y >= 0.0 && x < w as f64 && y < h as f64).then(|| to_count(y) * w + to_count(x))
                }));
            }
            let radius = (spot.area as f64 / std::f64::consts::PI).sqrt();
            let growth = DUST_GROWTH.max(to_count(radius));
            grow_into(&mut out, w, h, symmetric.into_iter(), growth);
        }
        Ok(out)
    }
}

/// A non-negative length rounded up to whole pixels.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // A spot's radius: positive, at most the frame's size.
fn to_count(v: f64) -> usize {
    v.ceil().max(0.0) as usize
}

/// `v` rounded to a whole offset.
#[allow(clippy::cast_possible_truncation)] // A ring radius: a few hundred pixels at most.
fn round(v: f64) -> isize {
    v.round() as isize
}

/// The lower median of `values`, or `None` when one isn't a number.
fn median(values: &mut [f32]) -> Option<f32> {
    if values.iter().any(|v| !v.is_finite()) || values.is_empty() {
        return None;
    }
    let mid = (values.len() - 1) / 2;
    let (_, &mut m, _) = values.select_nth_unstable_by(mid, f32::total_cmp);
    Some(m)
}

/// The frame's brightest color channel per pixel, linear; NaN for a pixel with a sample that isn't a number.
fn brightest(frame: &Image<f32>) -> Vec<f32> {
    let (w, c) = (frame.width(), frame.channels());
    let colors = match c {
        2 => 1,
        4 => 3,
        c => c,
    };
    let srgb = frame.color_space().encoding == Encoding::Srgb;
    let mut out = vec![0.0_f32; w * frame.height()];
    out.par_chunks_mut(w).zip(frame.par_rows()).for_each(|(out, row)| {
        for (o, pixel) in out.iter_mut().zip(row.chunks_exact(c)) {
            let mut m = f32::NEG_INFINITY;
            let mut finite = true;
            for &v in pixel.iter().take(colors) {
                finite &= v.is_finite();
                m = m.max(if srgb { srgb_to_linear(v) } else { v });
            }
            *o = if finite { m } else { f32::NAN };
        }
    });
    out
}

/// Detects the defects of frames in memory (see [`DefectDetector`]), on the frames [`detection_frames`] picks.
pub fn detect_defects(frames: &[Image<f32>], monitor: &dyn Monitor) -> Result<DefectMask, AlignError> {
    let first = frames.first().ok_or(AlignError::NoFrames)?;
    let mut detector = DefectDetector::new(first.width(), first.height())?;
    let picked = detection_frames(frames.len());
    for (done, &i) in picked.iter().enumerate() {
        let frame = frames.get(i).ok_or(AlignError::NoFrames)?;
        detector.add(frame, monitor)?;
        monitor.step(done as u64 + 1, picked.len() as u64)?;
    }
    detector.finish()
}

#[cfg(test)]
mod tests {
    use florastack_core::Unmonitored;

    use super::*;

    #[test]
    fn masks_read_black_spots_on_white_and_write_them_back() {
        let image = Image::from_fn(8, 6, 2, ColorSpace::SRGB, |x, y, c| if c == 1 || y != 3 || !(2..=3).contains(&x) { 1.0 } else { 0.0 }).unwrap();
        let mask = DefectMask::from_image(&image).unwrap();
        assert_eq!((mask.count(), mask.is_masked(2, 3), mask.is_masked(4, 3), mask.is_masked(99, 0)), (2, true, false, false));
        assert_eq!(mask.spots().collect::<Vec<_>>(), [Spot { x: 2, y: 3, width: 2, height: 1, area: 2 }]);
        let written = mask.to_image().unwrap();
        assert_eq!((written.get(2, 3, 0), written.get(0, 0, 0)), (Some(0), Some(255)));
        let back = DefectMask::from_image(&written.convert::<f32>().unwrap()).unwrap();
        assert_eq!(back, mask);
        let gray = Image::from_fn(4, 4, 4, ColorSpace::LINEAR, |_, _, c| if c == 3 { 0.0 } else { 0.6 }).unwrap();
        assert!(DefectMask::from_image(&gray).unwrap().is_empty(), "alpha doesn't count");
        let nan = Image::from_fn(4, 4, 1, ColorSpace::LINEAR, |x, _, _| if x == 0 { f32::NAN } else { 1.0 }).unwrap();
        assert_eq!(DefectMask::from_image(&nan).unwrap().count(), 4);
        assert!(matches!(mask.check_size(6, 8), Err(AlignError::MaskSize { width: 8, height: 6, frame_width: 6, frame_height: 8 })));
    }

    #[test]
    fn a_mask_of_too_many_spots_is_refused_before_it_takes_gigabytes() {
        // 250 000 single-pixel spots, every second pixel of every second row, as a hostile mask image could draw.
        let dots = DefectMask::from_fn(1000, 1000, |x, y| x % 2 == 0 && y % 2 == 0);
        assert!(matches!(dots, Err(AlignError::MaskSpots { max: MAX_SPOTS })));
        // A checkerboard touches diagonally: one spot.
        assert_eq!(DefectMask::from_fn(100, 100, |x, y| (x + y) % 2 == 0).unwrap().spots().len(), 1);
        let lines = DefectMask::from_fn(1000, 1000, |x, _| x % 2 == 0).unwrap();
        assert_eq!(lines.spots().len(), 500, "500 lines are 500 spots");
    }

    #[test]
    fn spots_are_8_connected_and_grow_within_a_disc() {
        let mask = DefectMask::from_fn(10, 10, |x, y| (x, y) == (1, 1) || (x, y) == (2, 2) || (x, y) == (7, 7)).unwrap();
        let spots: Vec<Spot> = mask.spots().collect();
        assert_eq!(spots, [Spot { x: 1, y: 1, width: 2, height: 2, area: 2 }, Spot { x: 7, y: 7, width: 1, height: 1, area: 1 }]);
        let grown = mask.grown(2).unwrap();
        assert!(grown.is_masked(9, 7) && grown.is_masked(7, 9) && !grown.is_masked(9, 9), "a disc, not a square");
        assert_eq!(grown.spots().count(), 2);
    }

    #[test]
    fn inpainting_fills_holes_smoothly_and_leaves_the_rest() {
        // A smooth ramp with a hole: the fill comes close to the ramp, and nothing else changes.
        let ramp = Image::from_fn(40, 30, 3, ColorSpace::LINEAR, |x, y, c| 0.2 + 0.01 * x as f32 + 0.005 * y as f32 + 0.1 * c as f32).unwrap();
        let mask = DefectMask::from_fn(40, 30, |x, y| (x as f32 - 20.0).hypot(y as f32 - 15.0) < 5.0).unwrap();
        let mut damaged = ramp.clone();
        for y in 0..30 {
            for x in 0..40 {
                if mask.is_masked(x, y) {
                    damaged.pixel_mut(x, y).unwrap().fill(0.0);
                }
            }
        }
        inpaint(&mut damaged, &mask).unwrap();
        let mut worst = 0.0_f32;
        for y in 0..30 {
            for x in 0..40 {
                for c in 0..3 {
                    let (a, b) = (damaged.get(x, y, c).unwrap(), ramp.get(x, y, c).unwrap());
                    if mask.is_masked(x, y) {
                        worst = worst.max((a - b).abs());
                    } else {
                        assert_eq!(a, b, "unmasked pixels are untouched");
                    }
                }
            }
        }
        assert!(worst < 0.03, "the fill follows the ramp: {worst}");
        // A mask of everything leaves the frame as it is; a mask of another size is an error.
        let all = DefectMask::from_fn(40, 30, |_, _| true).unwrap();
        let mut copy = ramp.clone();
        inpaint(&mut copy, &all).unwrap();
        assert_eq!(copy, ramp);
        assert!(inpaint(&mut copy, &DefectMask::empty(30, 40).unwrap()).is_err());
    }

    #[test]
    fn detection_needs_enough_frames_of_one_size() {
        let frame = Image::from_fn(32, 32, 3, ColorSpace::LINEAR, |_, _, _| 0.5).unwrap();
        let mut detector = DefectDetector::new(32, 32).unwrap();
        for _ in 0..4 {
            detector.add(&frame, &Unmonitored).unwrap();
        }
        assert!(matches!(detector.add(&Image::new(16, 32, 3, ColorSpace::LINEAR).unwrap(), &Unmonitored), Err(AlignError::FrameSize { .. })));
        assert_eq!(detector.finish(), Err(AlignError::FewFrames { frames: 4, min: MIN_DETECTION_FRAMES }));
        assert!(matches!(DefectDetector::new(8, 100), Err(AlignError::TooSmall { .. })));
        assert_eq!(detection_frames(3), [0, 1, 2]);
        let picked = detection_frames(100);
        assert_eq!((picked.len(), picked.first(), picked.last()), (32, Some(&0), Some(&99)));
    }
}
