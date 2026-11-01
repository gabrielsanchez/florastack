//! The depth-map method: for every pixel, the frame in which it is sharpest, read as a depth, and the composite made
//! by taking each pixel from the frames at its depth. Every output pixel is a blend of two adjacent frames' pixels at
//! the same place, so the method never changes color or contrast: its strength is smooth tones and accurate color,
//! where the pyramid method rebuilds the image from every frame's detail. It is the family of Zerene Stacker's `DMap`
//! and Helicon Focus's method B (the names only say which family; nothing of theirs is used).
//!
//! # Two passes
//!
//! The method reads every frame twice, and caches what lies between:
//!
//! 1. **Focus statistics** ([`Estimator`], pass 1, `focus.rs`): each frame's focus measure at every pixel, folded into
//!    a running record of the sharpest frame so far and its neighbors' focus, the best focus elsewhere in the stack,
//!    the least and the luminance at the sharpest. [`FocusStats`] holds the result: each pixel's sharpest frame,
//!    interpolated between frames, its confidence, and the guide image of the sharpest frames' luminance.
//! 2. **The depth map** ([`FocusStats::resolve`], `resolve.rs`): the pixels whose confidence is below the threshold,
//!    that a mask marks, or whose neighborhood is mostly below it, are left out; the rest of the depth is smoothed by
//!    an edge-aware filter guided by the guide image, and the pixels left out are filled from the confident ones
//!    around and from coarser scales. No frame is read: a new threshold or smoothing takes this step alone, which is
//!    what makes the threshold interactive. The result is a [`DepthMap`], with the confidence map.
//! 3. **The composite** ([`DepthMap::compositor`], pass 2, `composite.rs`): each frame read again, and each pixel of the
//!    output taken from the two frames on either side of its depth, blended by where the depth lies between them.
//!
//! [`depth_stack`] runs the three steps on frames in memory. The engine reads its frames from disk for each pass.
//!
//! # Focus and confidence
//!
//! The focus measure is the energy of the modified Laplacian, `|2Y - Y(x-1) - Y(x+1)| + |2Y - Y(y-1) - Y(y+1)|`, of
//! the frame's luminance blurred over [`DepthSettings::radius`], squared and averaged over a Gaussian window of the
//! same radius: S. K. Nayar and Y. Nakagawa's focus measure ("Shape from focus", IEEE Transactions on Pattern Analysis
//! and Machine Intelligence 16(8), 1994), which, unlike the Laplacian, doesn't cancel where the second derivatives
//! along the two axes have opposite signs. The blur before it measures detail at the radius's scale rather than the
//! pixel's: on camera frames at the pixel's scale, sensor noise has several times the energy of fine texture, and
//! picks the sharpest frame at random. The sharpest frame's index is refined between frames by fitting a Gaussian
//! through its focus and its two neighbors' (Nayar and Nakagawa's Gaussian interpolation of the focus measure).
//!
//! A pixel's confidence says how clearly its sharpest frame stands out: the local contrast of the sharpest frame
//! (the square root of its focus measure) less that of the runner-up, so that it is low where frames at different
//! depths are about as sharp and where the contrast is low; and none where the focus curve never falls far below its
//! peak, as the noise of a region without detail (an out-of-focus background, a smooth surface) doesn't (`focus.rs`).
//! The threshold ([`Threshold`]) is an absolute level of it, a percentile of the pixels, or, by default, a level
//! chosen from the statistics for unattended runs.
//!
//! # Memory
//!
//! Pass 1 keeps its running record tile by tile, as the other methods keep their accumulators (32 bytes per canvas
//! pixel); the statistics, the depth map and the confidence map are whole-canvas planes (4 bytes a pixel each), and
//! pass 2 keeps the blend's sums tile by tile (`8 C + 8` bytes a pixel for `C` channels).

mod composite;
mod contribution;
mod filter;
mod focus;
mod resolve;

use florastack_core::{Image, Monitor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use composite::CompositeKernel;
pub use contribution::Contributions;
pub use focus::{Estimator, FocusKernel, FocusStats};
pub use resolve::DepthMap;

use crate::average::MAX_RADIUS;
use crate::error::StackError;
use crate::method::{FrameProgress, StackShape, Stacked, Stacker};
use crate::source::FrameSource;
use crate::tiled::Tiling;

/// The method's name, as outputs and commands record it.
pub const DEPTH_MAP: &str = "depth-map";
/// The most frames a depth map can index: frame indices are kept as `f32`, exact up to here.
pub const MAX_FRAMES: usize = 1 << 24;
/// The largest `smoothing`, in pixels.
pub const MAX_SMOOTHING: f64 = 256.0;

/// How the contrast threshold is chosen ([`DepthSettings::threshold`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ThresholdMode {
    /// For unattended runs: where the depth of the pixels without detail follows the frames' noise, as on the smooth
    /// out-of-focus background of many macro subjects, the level that splits their confidence from the detail's;
    /// elsewhere the 10th percentile. See `resolve.rs`.
    #[default]
    Auto,
    /// The absolute level [`DepthSettings::level`].
    Level,
    /// The percentile [`DepthSettings::percentile`] of the pixels' confidence.
    Percentile,
}

/// The contrast threshold: below it, a pixel's sharpest frame is too uncertain to follow, and its depth is filled from
/// confident pixels around it. [`DepthSettings`] stores it as a mode and a value of each kind, so that a preset that
/// changes one of them merges with a project's settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Threshold {
    /// See [`ThresholdMode::Auto`].
    Auto,
    /// An absolute level of confidence, in units of linear luminance contrast (the modified Laplacian's), from 0: the
    /// level the interactive threshold sets.
    Level(f64),
    /// The share of the covered pixels, in percent from 0 to 100, whose depth is filled at least: the threshold is
    /// that percentile of their confidence, whatever the frames' contrast. The pixels without a clear peak, which have
    /// no confidence, are filled whatever the share.
    Percentile(f64),
}

impl Threshold {
    /// Checks the level or the percentile.
    pub fn validate(&self) -> Result<(), StackError> {
        match *self {
            Threshold::Level(v) if !(v.is_finite() && v >= 0.0) => {
                Err(StackError::Setting { name: "level", value: v, expected: "a level of at least 0" })
            }
            Threshold::Percentile(p) if !(p.is_finite() && (0.0..=100.0).contains(&p)) => {
                Err(StackError::Setting { name: "percentile", value: p, expected: "from 0 to 100 percent" })
            }
            _ => Ok(()),
        }
    }
}

/// The depth-map method's settings. See the module documentation. Every field has a default, so that a preset can name
/// only some.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct DepthSettings {
    /// The estimation radius: the standard deviation, in pixels, of the window the focus measure is averaged over,
    /// from 0 to 64. Larger is steadier on noise and smooth surfaces, smaller follows the edges between near and far
    /// parts more closely.
    #[schemars(range(min = 0.0, max = 64.0))]
    pub radius: f64,
    /// The smoothing radius: the reach, in pixels, of the edge-aware filter that smooths the depth map and carries it
    /// into the pixels below the threshold, from 0 to 256.
    #[schemars(range(min = 0.0, max = 256.0))]
    pub smoothing: f64,
    /// How the contrast threshold is chosen: `auto` (the default), `level` (an absolute level) or `percentile` (a
    /// share of the pixels).
    pub threshold: ThresholdMode,
    /// The threshold's level of confidence when `threshold` is `level`, from 0, in units of linear luminance contrast.
    #[schemars(range(min = 0.0))]
    pub level: f64,
    /// The share of the pixels, in percent from 0 to 100, whose depth is filled at least when `threshold` is
    /// `percentile`.
    #[schemars(range(min = 0.0, max = 100.0))]
    pub percentile: f64,
}

impl Default for DepthSettings {
    fn default() -> Self {
        DepthSettings { radius: 2.0, smoothing: 8.0, threshold: ThresholdMode::Auto, level: 0.001, percentile: 30.0 }
    }
}

impl DepthSettings {
    /// Checks every setting's range.
    pub fn validate(&self) -> Result<(), StackError> {
        if !(self.radius.is_finite() && (0.0..=MAX_RADIUS).contains(&self.radius)) {
            return Err(StackError::Setting { name: "radius", value: self.radius, expected: "from 0 to 64 pixels" });
        }
        if !(self.smoothing.is_finite() && (0.0..=MAX_SMOOTHING).contains(&self.smoothing)) {
            return Err(StackError::Setting { name: "smoothing", value: self.smoothing, expected: "from 0 to 256 pixels" });
        }
        Threshold::Level(self.level).validate()?;
        Threshold::Percentile(self.percentile).validate()
    }

    /// The contrast threshold these settings choose.
    pub fn contrast_threshold(&self) -> Threshold {
        match self.threshold {
            ThresholdMode::Auto => Threshold::Auto,
            ThresholdMode::Level => Threshold::Level(self.level),
            ThresholdMode::Percentile => Threshold::Percentile(self.percentile),
        }
    }

    /// These settings with `threshold` instead of their own.
    #[must_use]
    pub fn with_threshold(&self, threshold: Threshold) -> DepthSettings {
        let mut s = self.clone();
        match threshold {
            Threshold::Auto => s.threshold = ThresholdMode::Auto,
            Threshold::Level(level) => (s.threshold, s.level) = (ThresholdMode::Level, level),
            Threshold::Percentile(p) => (s.threshold, s.percentile) = (ThresholdMode::Percentile, p),
        }
        s
    }

    /// The same settings for frames reduced by `factor` (4 for a quarter-scale preview): the radii divided by it. A
    /// threshold given as a level stays: the reduced frames' contrast is about the same, their noise lower.
    #[must_use]
    pub fn reduced(&self, factor: f64) -> DepthSettings {
        let factor = if factor.is_finite() && factor > 0.0 { factor } else { 1.0 };
        DepthSettings { radius: self.radius / factor, smoothing: self.smoothing / factor, ..self.clone() }
    }

    /// A pass-1 stacker with these settings, tiled as `tiling` says.
    pub fn estimator(&self, tiling: &Tiling) -> Result<Estimator, StackError> {
        Estimator::new(self, tiling)
    }
}

/// What [`depth_stack`] makes: the composite, the depth map it was made from, and the focus statistics, from which
/// another threshold makes another depth map without reading the frames for pass 1 again.
#[derive(Clone, Debug, PartialEq)]
pub struct DepthStack {
    pub stacked: Stacked,
    pub map: DepthMap,
    pub stats: FocusStats,
}

/// The depth-map method on frames in memory, in order: pass 1, the depth map for `settings` (with the pixels `mask`
/// marks treated as low-confidence: one channel, the canvas's size, at least 0.5 to mark), and pass 2. Progress over
/// both passes goes to `monitor`.
pub fn depth_stack<F: FrameSource>(
    settings: &DepthSettings,
    tiling: &Tiling,
    frames: &[F],
    mask: Option<&Image<f32>>,
    monitor: &dyn Monitor,
) -> Result<DepthStack, StackError> {
    let first = frames.first().ok_or(StackError::NoFrames)?;
    let shape = StackShape::of(first);
    let n = frames.len() as u64;
    let mut estimator = settings.estimator(tiling)?;
    estimator.begin(shape)?;
    for (i, frame) in frames.iter().enumerate() {
        estimator.add_frame(frame, &FrameProgress { inner: monitor, frame: i as u64, frames: 2 * n })?;
    }
    let stats = estimator.finish(&Quiet(monitor))?;
    let map = stats.resolve(settings.contrast_threshold(), settings.smoothing, mask)?;
    let mut compositor = map.compositor(tiling)?;
    compositor.begin(shape)?;
    for (i, frame) in frames.iter().enumerate() {
        compositor.add_frame(frame, &FrameProgress { inner: monitor, frame: n + i as u64, frames: 2 * n })?;
    }
    let stacked = compositor.finish(&Quiet(monitor))?;
    Ok(DepthStack { stacked, map, stats })
}

/// A monitor that passes cancellation on but not progress.
struct Quiet<'a>(&'a dyn Monitor);

impl Monitor for Quiet<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }

    fn progress(&self, _: u64, _: u64) {}

    fn checkpoint(&self) -> Result<(), florastack_core::Cancelled> {
        self.0.checkpoint()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_are_checked_and_scale_for_previews() {
        assert!(DepthSettings::default().validate().is_ok());
        for bad in [
            DepthSettings { radius: -1.0, ..DepthSettings::default() },
            DepthSettings { radius: f64::NAN, ..DepthSettings::default() },
            DepthSettings { smoothing: 257.0, ..DepthSettings::default() },
            DepthSettings { level: -0.1, ..DepthSettings::default() },
            DepthSettings { level: f64::INFINITY, ..DepthSettings::default() },
            DepthSettings { percentile: 101.0, ..DepthSettings::default() },
        ] {
            assert!(matches!(bad.validate(), Err(StackError::Setting { .. })), "{bad:?}");
            assert!(bad.estimator(&Tiling::default()).is_err());
        }
        let quarter = DepthSettings::default().reduced(4.0);
        assert_eq!(quarter, DepthSettings { radius: 0.5, smoothing: 2.0, ..DepthSettings::default() });
        assert_eq!(DepthSettings::default().reduced(f64::NAN), DepthSettings::default());
        let level = DepthSettings::default().with_threshold(Threshold::Level(0.004));
        assert_eq!((level.threshold, level.level, level.contrast_threshold()), (ThresholdMode::Level, 0.004, Threshold::Level(0.004)));
        assert_eq!(level.with_threshold(Threshold::Auto).contrast_threshold(), Threshold::Auto);
        assert_eq!(DepthSettings::default().with_threshold(Threshold::Percentile(5.0)).contrast_threshold(), Threshold::Percentile(5.0));
    }

    #[test]
    fn settings_read_from_presets() {
        let toml: DepthSettings = toml::from_str("radius = 2.0\nthreshold = \"level\"\nlevel = 0.004").unwrap();
        assert_eq!(toml, DepthSettings { radius: 2.0, threshold: ThresholdMode::Level, level: 0.004, ..DepthSettings::default() });
        let json: DepthSettings = serde_json::from_str(r#"{ "threshold": "percentile", "percentile": 50 }"#).unwrap();
        assert_eq!(json.contrast_threshold(), Threshold::Percentile(50.0));
        assert!(serde_json::from_str::<DepthSettings>(r#"{ "threshold": { "level": 1 } }"#).is_err());
        assert!(serde_json::from_str::<DepthSettings>(r#"{ "threshold": "otsu" }"#).is_err());
        assert!(serde_json::from_str::<DepthSettings>(r#"{ "radios": 2 }"#).is_err());
    }
}
