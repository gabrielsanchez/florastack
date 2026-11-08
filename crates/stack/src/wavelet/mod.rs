//! The wavelet method: every frame split by a shift-invariant wavelet transform, and at every level and position the
//! detail of the frame with the most local activity kept. A decimated transform rings and blocks at edges because its
//! coefficients change with an image's shift, so the fine levels here are undecimated (O. Rockinger, "Image sequence
//! fusion using a shift-invariant wavelet transform", ICIP 1997).
//!
//! # The method
//!
//! A frame `x` is first split as `x = U + r`: `U` is its low-pass, the pyramid's REDUCE three times and EXPAND three
//! times back, and `r` what that leaves. Then:
//!
//! - `r` goes through the stationary wavelet transform ([`transform`]) for the finest [`crate::TILED_LEVELS`] levels, in tiles,
//!   undecimated: filters dilated by 1, 2 and 4, each level with horizontal, vertical and diagonal subbands.
//! - The 1/8-size low-pass goes through the same transform for the remaining levels, on the whole canvas, and is
//!   undecimated from there on. The transform would need tiles too large to be tiles at those levels (seven reach about
//!   520 pixels), and at that scale the one reduction has nothing left that rings.
//!
//! The result is `U` of the fused low-pass plus the fused `r`. With one frame it is that frame again, up to rounding.
//!
//! # The fusion rules
//!
//! 1. **Activity** ([`transform::activity`]): the energy of a level's three subbands together, on luminance, over a
//!    small binomial window of `window` samples. One decision covers the three subbands and every channel, so a pixel's
//!    color is one some frame has: no colored fringes.
//! 2. **Selection**: each position takes the frame with the most activity, weighed by the fourth power of its pooled
//!    coverage, as the pyramid's choices are. Ties keep the earlier frame. Blur barely lowers energy at coarse scales,
//!    so a level's own activity stops telling focus there: the levels above the whole-canvas part's second decide on
//!    that level's activity, pooled to their own scale (`select::deciding`), which groups the decision across scales
//!    (Z. Zhang and R. S. Blum, "A categorization of multiscale-decomposition-based image fusion schemes with a
//!    performance study for a digital camera application", Proc. IEEE 87(8), 1999).
//! 3. **Consistency** (`consistency`): each level's map of frames goes through a majority filter
//!    (`select::majority`), which replaces isolated picks, the ones noise makes, by the choice around them.
//! 4. **Grit** (`grit`): picking the largest of several frames' noise builds up grit at the finest level, so there, as
//!    in the pyramid, each frame is weighted by its activity raised to `1 / grit`. 0 picks, as the other levels do.
//! 5. **Low-pass** (`lowpass`): the coarsest low-pass is the frames', weighted by that same pooled activity to the
//!    power [`FOCUS_POWER`] and by coverage. A plain average lifts what only the sharp frames show dark: measured on real
//!    stacks, a ladybug's spot came out at 33 of 255 where its sharp frames have 15, about the frames' mean. Taking the
//!    frame chosen most often there, which `select` does, keeps it dark but mottles smooth backgrounds, where the
//!    choice is arbitrary and a hard edge between frames shows; the weights change gradually and have none.
//! 6. **Range** (`range_limit`): each pixel is held within the range of the frames that cover it, as in the pyramid,
//!    which removes the inversion halos selection can leave. [`crate::compress_range`] brings the unclipped result
//!    into `[0, 1]` as `tone` says.
//!
//! # Two passes
//!
//! The consistency filter replaces a pick by a neighbor's frame, whose coefficient a running best would already have
//! dropped, and filtering each frame's wins as they come would make a position depend on neighbors one pixel further
//! away with every frame, which tiles can't reproduce. So the method reads the frames twice, as the depth map does:
//! [`Selector`] keeps the best activity and its frame per level (`select.rs`), the maps are filtered once, whole, into
//! [`Choices`], and the compositor (`compose.rs`) reads every frame again and adds what it was chosen for. Synthesis is
//! linear, so that sum needs no coefficient planes, and memory doesn't grow with the frames. It is more than the
//! pyramid's all the same: on 20 MP frames the two peak at about 3.7 and 1.8 GB. Decisions need only luminance, so
//! pass 1 transforms one plane.
//!
//! # Tiles
//!
//! Tiles read the canvas from multiples of `2^TILED_LEVELS` pixels, as the pyramid's do, and every filter has fixed taps:
//! the result is the same bit for bit on one tile or many, one thread or many.
//!
//! # Speed
//!
//! Undecimated levels make the method's filters run on whole planes, and what limits it is memory traffic, not
//! arithmetic: on 8 cores it takes the same time on 4 threads as on 16, and with no page faults at all it still doesn't
//! scale. So it cuts the traffic: its tiles are wider than other methods' ([`TILE_SIZE`]), which halves the share of
//! their work that overlaps, and it reuses its planes ([`transform::Spare`]). Filtering in cache-sized bands of rows
//! is what would help next.

pub mod compose;
mod frame;
pub mod select;
pub mod transform;

use florastack_core::Monitor;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub use select::{Choices, Selector};
pub use transform::{Decomposition, Spare, Wavelet, decompose};

use crate::error::StackError;
use crate::method::{FrameProgress, Quiet, StackShape, Stacked, Stacker};
use crate::pyramid::{GRIT_RANGE, MAX_LEVELS, Tone};
use crate::source::FrameSource;
use crate::tiled::Tiling;

/// The method's name, as outputs and commands record it.
pub const WAVELET: &str = "wavelet";

/// The side of the method's tiles, unless the caller asks for another: on a 5 MP stack, 1024 took two thirds of the
/// time 512 did, and tiles of the whole canvas half, for more memory.
pub const TILE_SIZE: usize = 1024;

/// `tiling` with the method's tile size in place of the default one.
pub(crate) fn wider(tiling: &Tiling) -> Tiling {
    let tile_size = if tiling.tile_size == Tiling::DEFAULT_TILE_SIZE { TILE_SIZE } else { tiling.tile_size };
    Tiling { tile_size, ..tiling.clone() }
}

/// The power of a frame's pooled activity in [`Lowpass::Focus`]'s weights. On four real stacks, 1 and 2 left dark
/// objects lighter and 8 gained little over 4.
pub const FOCUS_POWER: i32 = 4;

/// How the coarsest low-pass is made.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Lowpass {
    /// The frames' weighted by their pooled activity at a coarse level, so that the frames in focus around a position
    /// set its brightness, with no edge between frames.
    #[default]
    Focus,
    /// The frames' average, weighted by coverage. Dark objects that only the sharp frames show dark come out lighter.
    Average,
    /// At each position, the frame chosen most often around it at the coarsest detail level.
    Select,
}

/// The wavelet method's settings. See the module documentation. Every field has a default, so that a preset can name
/// only some.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct WaveletSettings {
    /// The number of levels, from 1 to 10; 0 chooses from the canvas's size, as the pyramid does (5 for 720x540, 7 for
    /// 20 MP). More levels decide coarser structure by selection rather than by averaging the frames.
    #[schemars(range(max = 10))]
    pub levels: u32,
    /// The filter bank: `bior22`, symmetric, or `db2`.
    pub wavelet: Wavelet,
    /// The side of the window activity is measured over, 3 or 5 samples. 3 keeps a frame's choice closest to what it
    /// has in focus.
    #[schemars(range(min = 3, max = 5))]
    pub window: u32,
    /// Whether a majority filter replaces isolated picks in each level's choices.
    pub consistency: bool,
    /// How the coarsest low-pass is made: `focus`, `average` or `select`.
    pub lowpass: Lowpass,
    /// Grit suppression at the finest level, from 0.25 (closest to picking) to 1 (blends most), or 0 (off: the
    /// sharpest frame's coefficients are picked, as at the other levels).
    #[schemars(range(min = 0.0, max = 1.0))]
    pub grit: f64,
    /// How strictly each pixel is held within the range the frames span there, from 0 (off) to 1 (never darker or
    /// brighter than every frame).
    #[schemars(range(min = 0.0, max = 1.0))]
    pub range_limit: f64,
    /// How the result is brought into `[0, 1]`: `compress`, `clip` or `keep`.
    pub tone: Tone,
}

impl Default for WaveletSettings {
    fn default() -> Self {
        WaveletSettings {
            levels: 0,
            wavelet: Wavelet::Bior22,
            window: 3,
            consistency: true,
            lowpass: Lowpass::Focus,
            grit: 0.5,
            range_limit: 1.0,
            tone: Tone::Compress,
        }
    }
}

impl WaveletSettings {
    /// Checks every setting's range.
    pub fn validate(&self) -> Result<(), StackError> {
        if self.levels > MAX_LEVELS {
            return Err(StackError::Setting { name: "levels", value: f64::from(self.levels), expected: "from 0 (automatic) to 10" });
        }
        if self.window != 3 && self.window != 5 {
            return Err(StackError::Setting { name: "window", value: f64::from(self.window), expected: "3 or 5" });
        }
        #[allow(clippy::float_cmp)] // Exactly 0 is the documented "off".
        let grit_ok = self.grit == 0.0 || (self.grit.is_finite() && (GRIT_RANGE.0..=GRIT_RANGE.1).contains(&self.grit));
        if !grit_ok {
            return Err(StackError::Setting { name: "grit", value: self.grit, expected: "0 (off) or from 0.25 to 1" });
        }
        if !(self.range_limit.is_finite() && (0.0..=1.0).contains(&self.range_limit)) {
            return Err(StackError::Setting { name: "range_limit", value: self.range_limit, expected: "from 0 to 1" });
        }
        Ok(())
    }

    /// The finest level's blend power, `1 / grit`; `None` picks.
    pub(crate) fn power(&self) -> Option<f32> {
        #[allow(clippy::cast_possible_truncation)] // From 1 to 4.
        (self.grit > 0.0).then(|| (1.0 / self.grit) as f32)
    }

    /// The number of levels a `width` x `height` canvas gets: the setting, or the pyramid's automatic choice, and never
    /// more than halving the short side down to one sample allows.
    pub fn levels_for(&self, width: usize, height: usize) -> usize {
        let short = width.min(height).max(1);
        let most = (short.ilog2() as usize).max(1);
        let chosen = if self.levels == 0 { (short.ilog2() as usize).saturating_sub(4) } else { self.levels as usize };
        chosen.clamp(1, MAX_LEVELS as usize).min(most)
    }

    /// The same settings for frames reduced by `factor` (4 for a quarter-scale preview): an explicit number of levels
    /// loses one per halving of the frames' size; the other settings have no length.
    #[must_use]
    pub fn reduced(&self, factor: f64) -> WaveletSettings {
        let factor = if factor.is_finite() && factor > 1.0 { factor } else { 1.0 };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // log2 of a factor above 1, rounded, small.
        let fewer = factor.log2().round() as u32;
        let levels = if self.levels == 0 { 0 } else { self.levels.saturating_sub(fewer).max(1) };
        WaveletSettings { levels, ..self.clone() }
    }
}

/// The wavelet method on frames in memory, in order: pass 1, the choices, and pass 2. Progress over both passes goes to
/// `monitor`. The result is unclipped: [`crate::compress_range`] brings it into range.
pub fn wavelet_stack<F: FrameSource>(
    settings: &WaveletSettings,
    tiling: &Tiling,
    frames: &[F],
    monitor: &dyn Monitor,
) -> Result<Stacked, StackError> {
    let first = frames.first().ok_or(StackError::NoFrames)?;
    let shape = StackShape::of(first);
    let n = frames.len() as u64;
    let mut selector = Selector::new(settings, tiling)?;
    selector.begin(shape)?;
    for (i, frame) in frames.iter().enumerate() {
        selector.add_frame(frame, &FrameProgress { inner: monitor, frame: i as u64, frames: 2 * n })?;
    }
    let choices = Arc::new(selector.finish(&Quiet(monitor))?);
    let mut compositor = choices.compositor(tiling)?;
    compositor.begin(shape)?;
    for (i, frame) in frames.iter().enumerate() {
        compositor.add_frame(frame, &FrameProgress { inner: monitor, frame: n + i as u64, frames: 2 * n })?;
    }
    compositor.finish(&Quiet(monitor))
}
