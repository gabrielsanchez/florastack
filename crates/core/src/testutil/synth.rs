//! The synthetic stack generator: a sharp image and a depth map in, a focus stack with known ground truth out.
//!
//! Frame `i` of `n` is focused at a depth between [`StackParams::focus_range`]'s ends (evenly spaced, first to last)
//! and is made in the order a camera would:
//!
//! 1. **Defocus**: each pixel is blurred with a Gaussian whose standard deviation grows linearly with the distance
//!    between its depth and the frame's focus depth (`blur_per_depth` pixels per unit of depth, at most `max_blur`), a
//!    thin-lens approximation. Blur levels `0, blur_step, 2 blur_step, ...` are computed once per stack and each pixel
//!    interpolates between the two levels around its own sigma. This gathers the blur per pixel and so doesn't model
//!    occlusion (an out-of-focus edge spreading over its background); the scene's edges still blur realistically.
//! 2. **Geometry**: a similarity transform about the image center, `transform = scale, then rotate, then shift`
//!    (see [`Affine::similarity`]): focus breathing scales linearly from 1 (first frame) to `1 + breathing` (last), and
//!    each frame gets a random sub-pixel shift and rotation. The frame samples the defocused scene at
//!    `transform.inverse()` of each pixel with a bicubic filter, repeating the edge pixels outside the scene.
//! 3. **Brightness**: a random gain in `[1 - brightness_jitter, 1 + brightness_jitter]`.
//! 4. **Dust**: fixed spots in sensor coordinates, the same in every frame, that darken by up to their opacity.
//! 5. **Noise**: Gaussian read noise plus shot noise whose variance grows with the signal. Samples are not clipped.
//!
//! Each step is skipped when its parameters make it a no-op, so [`StackParams::identity`] returns exact copies of the
//! sharp image. The [`GroundTruth`] keeps the sharp image, the depth map, each frame's focus depth, transform and
//! gain, and the dust spots: alignment can be scored with [`GroundTruth::relative`] and [`transform_error`], and
//! stacking against the sharp image with florastack-metrics. Everything is deterministic for a given seed.

use super::ops::{gaussian_blur, to_f32, to_index, warp};
use super::rng::Rng;
use crate::geometry::Affine;
use crate::image::{Image, ImageError};

/// The most frames one stack may have.
pub const MAX_FRAMES: usize = 10_000;
/// The largest blur, in pixels of standard deviation, that `max_blur` may ask for.
pub const MAX_BLUR: f32 = 64.0;
/// The most blur levels one stack may need (`max_blur / blur_step`).
const MAX_LEVELS: usize = 512;

/// A dust spot on the sensor: a soft round shadow centered on `(x, y)` (frame pixels) that darkens by `opacity`
/// at its center and falls off as a Gaussian of standard deviation `radius` pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DustSpot {
    pub x: f32,
    pub y: f32,
    pub radius: f32,
    pub opacity: f32,
}

impl DustSpot {
    /// `count` spots at random places in a `width` x `height` frame, with radii of 1.5 to 4 pixels and opacities of
    /// 0.2 to 0.6.
    pub fn random(count: usize, width: usize, height: usize, seed: u64) -> Vec<DustSpot> {
        let mut rng = Rng::new(seed);
        (0..count)
            .map(|_| DustSpot {
                x: to_f32(rng.range(0.0, width as f64)),
                y: to_f32(rng.range(0.0, height as f64)),
                radius: to_f32(rng.range(1.5, 4.0)),
                opacity: to_f32(rng.range(0.2, 0.6)),
            })
            .collect()
    }

    /// The factor this spot multiplies pixel `(x, y)` by.
    pub fn transmission(&self, x: f64, y: f64) -> f64 {
        let r2 = (x - f64::from(self.x)).powi(2) + (y - f64::from(self.y)).powi(2);
        1.0 - f64::from(self.opacity) * (-r2 / (2.0 * f64::from(self.radius).powi(2))).exp()
    }
}

/// How to make a stack. Every length is in pixels and every depth in the depth map's units.
#[derive(Clone, Debug, PartialEq)]
pub struct StackParams {
    /// The number of frames, 1 to [`MAX_FRAMES`].
    pub frames: usize,
    /// The focus depths of the first and the last frame; `None` takes the depth map's minimum and maximum.
    pub focus_range: Option<(f32, f32)>,
    /// Gaussian blur, as a standard deviation in pixels, per unit of distance between a pixel's depth and the focus.
    pub blur_per_depth: f32,
    /// The most blur any pixel gets, at most [`MAX_BLUR`].
    pub max_blur: f32,
    /// The spacing of the precomputed blur levels; smaller is more accurate and slower.
    pub blur_step: f32,
    /// Focus breathing: the last frame's magnification relative to the first, minus one (0.02 is 2% larger). Between
    /// -0.5 and 0.5.
    pub breathing: f32,
    /// The largest random shift along each axis.
    pub max_shift: f32,
    /// The largest random rotation, in degrees.
    pub max_rotation_deg: f32,
    /// The largest random relative brightness change.
    pub brightness_jitter: f32,
    /// Read noise: the standard deviation of Gaussian noise added to every sample (linear units, 1 = white).
    pub read_noise: f32,
    /// Shot noise: Gaussian noise with variance `shot_noise^2 * signal`.
    pub shot_noise: f32,
    /// Dust on the sensor, the same in every frame.
    pub dust: Vec<DustSpot>,
    /// Seeds the transforms, gains and noise.
    pub seed: u64,
}

impl StackParams {
    /// Every effect off: each frame is an exact copy of the sharp image, with identity transforms and unit gains.
    pub fn identity(frames: usize) -> StackParams {
        StackParams {
            frames,
            focus_range: None,
            blur_per_depth: 0.0,
            max_blur: 0.0,
            blur_step: 0.5,
            breathing: 0.0,
            max_shift: 0.0,
            max_rotation_deg: 0.0,
            brightness_jitter: 0.0,
            read_noise: 0.0,
            shot_noise: 0.0,
            dust: Vec::new(),
            seed: 0,
        }
    }

    /// A clean stack: defocus up to 8 px, 2% focus breathing, shifts of up to 1.5 px and rotations of up to 0.3
    /// degrees, with no brightness change, noise or dust.
    pub fn clean(frames: usize, seed: u64) -> StackParams {
        StackParams {
            blur_per_depth: 8.0,
            max_blur: 8.0,
            breathing: 0.02,
            max_shift: 1.5,
            max_rotation_deg: 0.3,
            seed,
            ..StackParams::identity(frames)
        }
    }

    /// [`StackParams::clean`] plus 3% brightness jitter, sensor noise (read 0.005, shot 0.01) and six dust spots in a
    /// `width` x `height` frame.
    pub fn noisy_dusty(frames: usize, width: usize, height: usize, seed: u64) -> StackParams {
        StackParams {
            brightness_jitter: 0.03,
            read_noise: 0.005,
            shot_noise: 0.01,
            dust: DustSpot::random(6, width, height, seed ^ 0xd057),
            ..StackParams::clean(frames, seed)
        }
    }

    fn validate(&self) -> Result<(), SynthError> {
        if self.frames == 0 || self.frames > MAX_FRAMES {
            return Err(SynthError::Frames(self.frames));
        }
        let non_negative = [
            ("blur_per_depth", self.blur_per_depth),
            ("max_blur", self.max_blur),
            ("max_shift", self.max_shift),
            ("max_rotation_deg", self.max_rotation_deg),
            ("brightness_jitter", self.brightness_jitter),
            ("read_noise", self.read_noise),
            ("shot_noise", self.shot_noise),
        ];
        for (name, value) in non_negative {
            if !(value.is_finite() && value >= 0.0) {
                return Err(SynthError::Param { name, value });
            }
        }
        let checks = [
            ("max_blur", self.max_blur <= MAX_BLUR, self.max_blur),
            ("blur_step", self.blur_step.is_finite() && self.blur_step > 0.0, self.blur_step),
            ("breathing", self.breathing.is_finite() && self.breathing.abs() <= 0.5, self.breathing),
            ("brightness_jitter", self.brightness_jitter < 1.0, self.brightness_jitter),
        ];
        if let Some(&(name, _, value)) = checks.iter().find(|(_, ok, _)| !ok) {
            return Err(SynthError::Param { name, value });
        }
        if let Some((near, far)) = self.focus_range
            && let Some(&value) = [near, far].iter().find(|v| !v.is_finite())
        {
            return Err(SynthError::Param { name: "focus_range", value });
        }
        for spot in &self.dust {
            let ok = spot.x.is_finite() && spot.y.is_finite() && spot.radius.is_finite() && spot.radius > 0.0 && (0.0..=1.0).contains(&spot.opacity);
            if !ok {
                return Err(SynthError::Dust(*spot));
            }
        }
        Ok(())
    }
}

/// Why a stack can't be made.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum SynthError {
    #[error("a stack has 1 to {MAX_FRAMES} frames, not {0}")]
    Frames(usize),
    #[error("{name} is out of range: {value}")]
    Param { name: &'static str, value: f32 },
    #[error("a dust spot needs a finite position, a positive radius and an opacity from 0 to 1: {0:?}")]
    Dust(DustSpot),
    #[error(
        "the depth map must have one channel and the sharp image's size ({width}x{height}), not {depth_width}x{depth_height} with {depth_channels} channels"
    )]
    DepthShape { width: usize, height: usize, depth_width: usize, depth_height: usize, depth_channels: usize },
    #[error("the depth map holds a value that is not finite")]
    DepthValue,
    #[error("blurring up to {sigma} px in steps of {step} px needs more than {MAX_LEVELS} levels: raise blur_step")]
    Levels { sigma: f32, step: f32 },
    #[error(transparent)]
    Image(#[from] ImageError),
}

/// The truth about one frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameTruth {
    /// The depth this frame is focused at.
    pub focus_depth: f32,
    /// Maps sharp-image (scene) coordinates to this frame's coordinates.
    pub transform: Affine,
    /// The terms of `transform`: magnification, rotation in degrees and shift in pixels.
    pub scale: f64,
    pub rotation_deg: f64,
    pub shift: (f64, f64),
    /// The brightness gain applied to this frame.
    pub gain: f32,
}

/// Everything known about a synthetic stack.
#[derive(Clone, Debug, PartialEq)]
pub struct GroundTruth {
    /// The all-in-focus image a perfect stack would produce (in scene coordinates).
    pub sharp: Image<f32>,
    pub depth: Image<f32>,
    pub frames: Vec<FrameTruth>,
    pub dust: Vec<DustSpot>,
}

impl GroundTruth {
    /// The transform from frame `master`'s coordinates to frame `frame`'s: what aligning `frame` to `master`
    /// recovers. `None` for an index out of range.
    pub fn relative(&self, frame: usize, master: usize) -> Option<Affine> {
        let to_frame = self.frames.get(frame)?.transform;
        let from_master = self.frames.get(master)?.transform.inverse()?;
        Some(from_master.then(&to_frame))
    }

    /// The frame whose focus depth is closest to the depth of scene pixel `(x, y)`: where that pixel is sharpest.
    pub fn sharpest_frame(&self, x: usize, y: usize) -> Option<usize> {
        let depth = self.depth.get(x, y, 0)?;
        let distance = |f: &FrameTruth| (f.focus_depth - depth).abs();
        (0..self.frames.len()).min_by(|&a, &b| distance(&self.frames[a]).total_cmp(&distance(&self.frames[b])))
    }
}

/// A synthetic focus stack and its ground truth.
#[derive(Clone, Debug, PartialEq)]
pub struct SyntheticStack {
    /// The frames in focus order, each the size of the sharp image, linear, with packed rows.
    pub frames: Vec<Image<f32>>,
    pub truth: GroundTruth,
}

/// The mean distance in pixels between where `estimated` and `truth` put the points of an 8x8 grid spread over a
/// `width` x `height` frame: the alignment error measure of the roadmap's accuracy targets.
pub fn transform_error(estimated: &Affine, truth: &Affine, width: usize, height: usize) -> f64 {
    const N: usize = 8;
    let (w, h) = ((width.max(1) - 1) as f64, (height.max(1) - 1) as f64);
    let mut sum = 0.0;
    for j in 0..N {
        for i in 0..N {
            let (x, y) = (w * i as f64 / (N - 1) as f64, h * j as f64 / (N - 1) as f64);
            let (a, b) = (estimated.apply(x, y), truth.apply(x, y));
            sum += (a.0 - b.0).hypot(a.1 - b.1);
        }
    }
    sum / (N * N) as f64
}

/// Makes a focus stack from `sharp` (any channel count, linear) and `depth` (one channel, the same size).
pub fn generate(sharp: &Image<f32>, depth: &Image<f32>, params: &StackParams) -> Result<SyntheticStack, SynthError> {
    params.validate()?;
    let (w, h, c) = (sharp.width(), sharp.height(), sharp.channels());
    if depth.channels() != 1 || depth.width() != w || depth.height() != h {
        return Err(SynthError::DepthShape {
            width: w,
            height: h,
            depth_width: depth.width(),
            depth_height: depth.height(),
            depth_channels: depth.channels(),
        });
    }
    let depths: Vec<f32> = depth.rows().flatten().copied().collect();
    if depths.iter().any(|d| !d.is_finite()) {
        return Err(SynthError::DepthValue);
    }
    let (lowest, highest) = depths.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), &d| (lo.min(d), hi.max(d)));
    let (near, far) = params.focus_range.unwrap_or((lowest, highest));
    let n = params.frames;
    let progress = |i: usize| if n == 1 { 0.0 } else { i as f64 / (n - 1) as f64 };

    // The transforms and gains, drawn in frame order whatever the other parameters, so that turning one effect on
    // doesn't change the others.
    let mut rng = Rng::new(params.seed);
    let center = ((w - 1) as f64 / 2.0, (h - 1) as f64 / 2.0);
    let truths: Vec<FrameTruth> = (0..n)
        .map(|i| {
            let t = progress(i);
            let max_shift = f64::from(params.max_shift);
            let shift = (rng.range(-max_shift, max_shift), rng.range(-max_shift, max_shift));
            let rotation_deg = rng.range(-f64::from(params.max_rotation_deg), f64::from(params.max_rotation_deg));
            let jitter = f64::from(params.brightness_jitter);
            let gain = to_f32(1.0 + rng.range(-jitter, jitter));
            let scale = 1.0 + f64::from(params.breathing) * t;
            FrameTruth {
                focus_depth: to_f32(f64::from(near) + (f64::from(far) - f64::from(near)) * t),
                transform: Affine::similarity(center, scale, rotation_deg.to_radians(), shift),
                scale,
                rotation_deg,
                shift,
                gain,
            }
        })
        .collect();

    // The blur levels every frame interpolates between.
    let base = packed(sharp)?;
    let max_defocus = truths.iter().map(|f| (f.focus_depth - lowest).abs().max((f.focus_depth - highest).abs())).fold(0.0, f32::max);
    let sigma_needed = (params.blur_per_depth * max_defocus).min(params.max_blur);
    let level_count = if sigma_needed > 0.0 { to_index(f64::from(sigma_needed / params.blur_step).floor()).saturating_add(2) } else { 1 };
    if level_count > MAX_LEVELS {
        return Err(SynthError::Levels { sigma: sigma_needed, step: params.blur_step });
    }
    let levels = (0..level_count)
        .map(|l| if l == 0 { Ok(base.clone()) } else { gaussian_blur(&base, l as f64 * f64::from(params.blur_step)) })
        .collect::<Result<Vec<_>, _>>()?;

    let mut frames = Vec::with_capacity(n);
    for (i, truth) in truths.iter().enumerate() {
        let mut frame = if level_count == 1 { base.clone() } else { defocus(&levels, &depths, truth.focus_depth, params)? };
        if !truth.transform.is_identity() {
            // A similarity transform with a scale in [0.5, 1.5] is invertible.
            let inverse = truth.transform.inverse().unwrap_or(Affine::IDENTITY);
            frame = warp(&frame, &inverse)?;
        }
        if !is_one(truth.gain) {
            for v in frame.rows_mut().flatten() {
                *v *= truth.gain;
            }
        }
        apply_dust(&mut frame, &params.dust);
        if params.read_noise > 0.0 || params.shot_noise > 0.0 {
            let mut noise = Rng::new(params.seed ^ (0x6e6f_6973_6500_0000 + i as u64));
            let (read, shot) = (f64::from(params.read_noise), f64::from(params.shot_noise));
            for v in frame.rows_mut().flatten() {
                let signal = f64::from(*v);
                *v = to_f32(signal + read * noise.normal() + shot * signal.max(0.0).sqrt() * noise.normal());
            }
        }
        debug_assert_eq!((frame.width(), frame.height(), frame.channels()), (w, h, c));
        frames.push(frame);
    }
    Ok(SyntheticStack { frames, truth: GroundTruth { sharp: sharp.clone(), depth: depth.clone(), frames: truths, dust: params.dust.clone() } })
}

/// A copy of `image` with packed rows.
fn packed(image: &Image<f32>) -> Result<Image<f32>, ImageError> {
    let data: Vec<f32> = image.rows().flatten().copied().collect();
    Image::from_vec(image.width(), image.height(), image.channels(), image.color_space().clone(), data)
}

/// The scene defocused for a frame focused at `focus`: each pixel interpolated between the blur levels around its
/// own sigma. The levels have packed rows.
fn defocus(levels: &[Image<f32>], depths: &[f32], focus: f32, params: &StackParams) -> Result<Image<f32>, ImageError> {
    let Some(first) = levels.first() else {
        return Err(ImageError::Empty { width: 0, height: 0 });
    };
    let c = first.channels();
    let raw: Vec<&[f32]> = levels.iter().map(Image::as_raw).collect();
    let last = raw.len() - 1;
    let mut data = Vec::with_capacity(first.as_raw().len());
    for (p, &d) in depths.iter().enumerate() {
        let sigma = (params.blur_per_depth * (d - focus).abs()).min(params.max_blur);
        let position = f64::from(sigma / params.blur_step);
        let lo = to_index(position.floor()).min(last);
        let t = to_f32(position - lo as f64);
        let samples = p * c..p * c + c;
        let below = &raw[lo][samples.clone()];
        if t > 0.0 && lo < last {
            data.extend(below.iter().zip(&raw[lo + 1][samples]).map(|(b, a)| b + t * (a - b)));
        } else {
            data.extend_from_slice(below);
        }
    }
    Image::from_vec(first.width(), first.height(), c, first.color_space().clone(), data)
}

/// Whether a gain is exactly 1, so that applying it can be skipped and an untouched frame stays bit-exact.
#[allow(clippy::float_cmp)] // Exact on purpose: any other gain is applied.
fn is_one(gain: f32) -> bool {
    gain == 1.0
}

/// Darkens `frame` under each dust spot (within four radii of its center, where the shadow is visible).
fn apply_dust(frame: &mut Image<f32>, dust: &[DustSpot]) {
    let (w, h) = (frame.width(), frame.height());
    for spot in dust {
        let reach = 4.0 * f64::from(spot.radius);
        let (cx, cy) = (f64::from(spot.x), f64::from(spot.y));
        let x0 = to_index((cx - reach).floor().max(0.0));
        let y0 = to_index((cy - reach).floor().max(0.0));
        let x1 = to_index((cx + reach).ceil().max(0.0)).min(w - 1);
        let y1 = to_index((cy + reach).ceil().max(0.0)).min(h - 1);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let factor = to_f32(spot.transmission(x as f64, y as f64));
                if let Some(pixel) = frame.pixel_mut(x, y) {
                    for v in pixel {
                        *v *= factor;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::ops::gaussian_blur;
    use super::super::scene;
    use super::*;
    use crate::image::ColorSpace;

    fn test_scene(w: usize, h: usize) -> (Image<f32>, Image<f32>) {
        scene::textured(w, h, 11).unwrap()
    }

    fn flat_depth(w: usize, h: usize, d: f32) -> Image<f32> {
        Image::from_fn(w, h, 1, ColorSpace::LINEAR, |_, _, _| d).unwrap()
    }

    #[test]
    fn zero_blur_and_zero_transform_return_the_input() {
        let (sharp, depth) = test_scene(48, 32);
        let stack = generate(&sharp, &depth, &StackParams::identity(5)).unwrap();
        assert_eq!(stack.frames.len(), 5);
        for (frame, truth) in stack.frames.iter().zip(&stack.truth.frames) {
            assert_eq!(frame, &sharp, "bit-exact copy");
            assert!(truth.transform.is_identity());
            assert_eq!((truth.gain, truth.scale, truth.rotation_deg, truth.shift), (1.0, 1.0, 0.0, (0.0, 0.0)));
        }
        assert_eq!(stack.truth.sharp, sharp);
        assert_eq!(stack.truth.depth, depth);
    }

    #[test]
    fn a_strided_input_gives_the_same_stack() {
        let (sharp, depth) = test_scene(20, 10);
        let mut padded = Vec::new();
        for row in sharp.rows() {
            padded.extend_from_slice(row);
            padded.extend_from_slice(&[f32::NAN; 5]);
        }
        let strided = Image::from_vec_with_stride(20, 10, 3, 65, ColorSpace::LINEAR, padded).unwrap();
        let params = StackParams::noisy_dusty(3, 20, 10, 5);
        let a = generate(&sharp, &depth, &params).unwrap();
        let b = generate(&strided, &depth, &params).unwrap();
        assert_eq!(a.frames, b.frames);
        assert!(b.frames.iter().all(|f| f.as_raw().iter().all(|v| v.is_finite())), "the padding never leaks in");
    }

    #[test]
    fn stacks_are_deterministic_for_a_seed() {
        let (sharp, depth) = test_scene(32, 24);
        let params = StackParams::noisy_dusty(4, 32, 24, 9);
        assert_eq!(generate(&sharp, &depth, &params).unwrap(), generate(&sharp, &depth, &params).unwrap());
        let other = generate(&sharp, &depth, &StackParams { seed: 10, ..params.clone() }).unwrap();
        assert_ne!(other.frames, generate(&sharp, &depth, &params).unwrap().frames);
    }

    #[test]
    fn defocus_grows_with_the_distance_from_the_focus() {
        let (sharp, _) = test_scene(40, 30);
        // Everything at depth 0.5, frames focused at 0, 0.25 and 0.5: sigma 2, 1 and 0 px, all exact levels.
        let depth = flat_depth(40, 30, 0.5);
        let params = StackParams { focus_range: Some((0.0, 0.5)), blur_per_depth: 4.0, max_blur: 8.0, ..StackParams::identity(3) };
        let stack = generate(&sharp, &depth, &params).unwrap();
        assert_eq!(stack.frames[2], sharp, "in focus: untouched");
        for (frame, sigma) in stack.frames.iter().zip([2.0, 1.0]) {
            let expected = gaussian_blur(&sharp, sigma).unwrap();
            assert!(frame.as_raw().iter().zip(expected.as_raw()).all(|(a, b)| (a - b).abs() < 1e-6), "sigma {sigma}");
        }
        // Halfway between two levels, a pixel lies between them.
        let between = generate(&sharp, &depth, &StackParams { blur_step: 2.0, ..params.clone() }).unwrap();
        let (lo, hi) = (sharp.get(7, 9, 1).unwrap(), gaussian_blur(&sharp, 2.0).unwrap().get(7, 9, 1).unwrap());
        assert!((between.frames[1].get(7, 9, 1).unwrap() - f32::midpoint(lo, hi)).abs() < 1e-6);
        // In a real scene the sharpest frame for a pixel is the one focused at its depth.
        let (sharp, depth) = test_scene(40, 30);
        let stack = generate(&sharp, &depth, &StackParams { blur_per_depth: 6.0, max_blur: 6.0, ..StackParams::identity(5) }).unwrap();
        let best = stack.truth.sharpest_frame(3, 29).unwrap();
        assert!((stack.truth.frames[best].focus_depth - depth.get(3, 29, 0).unwrap()).abs() <= 0.125 + 1e-6);
    }

    #[test]
    #[allow(clippy::float_cmp)] // The first frame's scale is exactly 1 by construction.
    fn frames_follow_their_true_transforms() {
        // A smooth scene that bicubic sampling reproduces closely, so frame(T(p)) can be compared with scene(p).
        let f = |x: f64, y: f64| 0.5 + 0.2 * (x / 7.0).sin() * (y / 9.0).cos();
        let sharp = Image::from_fn(64, 48, 1, ColorSpace::LINEAR, |x, y, _| to_f32(f(x as f64, y as f64))).unwrap();
        let depth = flat_depth(64, 48, 0.0);
        let params = StackParams { breathing: 0.05, max_shift: 2.0, max_rotation_deg: 1.0, seed: 3, ..StackParams::identity(4) };
        let stack = generate(&sharp, &depth, &params).unwrap();
        let last = stack.truth.frames[3];
        assert!((last.scale - (1.0 + f64::from(params.breathing))).abs() < 1e-12 && stack.truth.frames[0].scale == 1.0);
        for (frame, truth) in stack.frames.iter().zip(&stack.truth.frames) {
            assert!(truth.shift.0.abs() <= 2.0 && truth.rotation_deg.abs() <= 1.0);
            let inverse = truth.transform.inverse().unwrap();
            for (x, y) in [(20, 15), (32, 24), (45, 30)] {
                let (sx, sy) = inverse.apply(x as f64, y as f64);
                let got = f64::from(frame.get(x, y, 0).unwrap());
                assert!((got - f(sx, sy)).abs() < 2e-3, "({x}, {y}): {got} vs {}", f(sx, sy));
            }
        }
        // Aligning frame 3 to frame 1 recovers T3 after the inverse of T1.
        let relative = stack.truth.relative(3, 1).unwrap();
        let (x, y) = stack.truth.frames[1].transform.apply(10.0, 20.0);
        let expected = last.transform.apply(10.0, 20.0);
        let got = relative.apply(x, y);
        assert!((got.0 - expected.0).abs() < 1e-9 && (got.1 - expected.1).abs() < 1e-9);
        assert!((stack.truth.relative(2, 2).unwrap().apply(5.0, 6.0).0 - 5.0).abs() < 1e-9);
        assert_eq!(stack.truth.relative(4, 0), None);
    }

    #[test]
    #[allow(clippy::float_cmp)] // A gain multiplies each sample once, so the result is exact.
    fn brightness_dust_and_noise_are_applied_as_specified() {
        let sharp = Image::from_fn(64, 64, 3, ColorSpace::LINEAR, |_, _, c| 0.25 + 0.25 * c as f32).unwrap();
        let depth = flat_depth(64, 64, 0.0);
        let gains = generate(&sharp, &depth, &StackParams { brightness_jitter: 0.1, seed: 4, ..StackParams::identity(3) }).unwrap();
        for (frame, truth) in gains.frames.iter().zip(&gains.truth.frames) {
            assert!((0.9..=1.1).contains(&truth.gain));
            assert!(frame.as_raw().iter().zip(sharp.as_raw()).all(|(a, b)| *a == b * truth.gain));
        }
        let spot = DustSpot { x: 20.0, y: 30.0, radius: 2.0, opacity: 0.5 };
        let dusty = generate(&sharp, &depth, &StackParams { dust: vec![spot], ..StackParams::identity(2) }).unwrap();
        assert_eq!(dusty.frames[0], dusty.frames[1], "dust stays put");
        assert!((dusty.frames[0].get(20, 30, 2).unwrap() - 0.75 * 0.5).abs() < 1e-6);
        assert_eq!(dusty.frames[0].get(40, 30, 2), Some(0.75), "far from the spot nothing changes");
        let noisy = generate(&sharp, &depth, &StackParams { read_noise: 0.02, ..StackParams::identity(2) }).unwrap();
        let diff: Vec<f64> = noisy.frames[0].as_raw().iter().zip(sharp.as_raw()).map(|(a, b)| f64::from(a - b)).collect();
        let mean = diff.iter().sum::<f64>() / diff.len() as f64;
        let sd = (diff.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / diff.len() as f64).sqrt();
        assert!(mean.abs() < 0.002 && (sd - 0.02).abs() < 0.002, "{mean} {sd}");
        assert_ne!(noisy.frames[0], noisy.frames[1], "each frame has its own noise");
    }

    #[test]
    fn transform_error_is_the_mean_point_distance() {
        let t = Affine::similarity((10.0, 10.0), 1.01, 0.01, (0.3, -0.2));
        assert_eq!(transform_error(&t, &t, 64, 48), 0.0);
        let shifted = t.then(&Affine::translation(0.0, 0.1));
        assert!((transform_error(&shifted, &t, 64, 48) - 0.1).abs() < 1e-12);
    }

    #[test]
    fn bad_parameters_are_errors() {
        let (sharp, depth) = test_scene(16, 16);
        let run = |params: StackParams| generate(&sharp, &depth, &params).unwrap_err();
        assert_eq!(run(StackParams::identity(0)), SynthError::Frames(0));
        assert!(matches!(run(StackParams { max_shift: f32::NAN, ..StackParams::identity(2) }), SynthError::Param { name: "max_shift", .. }));
        assert!(matches!(run(StackParams { blur_step: 0.0, ..StackParams::identity(2) }), SynthError::Param { name: "blur_step", .. }));
        assert!(matches!(run(StackParams { max_blur: 100.0, ..StackParams::identity(2) }), SynthError::Param { name: "max_blur", .. }));
        assert!(matches!(
            run(StackParams { blur_per_depth: 64.0, max_blur: 64.0, blur_step: 0.01, ..StackParams::identity(2) }),
            SynthError::Levels { .. }
        ));
        let bad_spot = DustSpot { x: 1.0, y: 1.0, radius: 0.0, opacity: 0.5 };
        assert_eq!(run(StackParams { dust: vec![bad_spot], ..StackParams::identity(2) }), SynthError::Dust(bad_spot));
        let small = flat_depth(8, 16, 0.5);
        assert!(matches!(generate(&sharp, &small, &StackParams::identity(1)), Err(SynthError::DepthShape { depth_width: 8, .. })));
        let nan = flat_depth(16, 16, f32::NAN);
        assert_eq!(generate(&sharp, &nan, &StackParams::identity(1)), Err(SynthError::DepthValue));
    }
}
