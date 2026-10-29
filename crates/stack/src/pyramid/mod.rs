//! The pyramid method: every frame decomposed into a Laplacian pyramid (Burt and Adelson), and at every level and
//! position the coefficient of the frame with the most local energy kept, so that the finest structure of every frame
//! survives where near and far detail overlap: hair, bristles, antennae over a textured background. It is the family of
//! Zerene Stacker's `PMax` and Helicon Focus's method C (the names only say which family; nothing of theirs is used).
//!
//! # The method
//!
//! For each frame, on each tile ([`Pyramid`], a [`crate::TileKernel`]):
//!
//! 1. **Pyramid**: the frame's Gaussian pyramid by REDUCE with the 5-tap binomial kernel, by normalized convolution
//!    where the frame covers only part of the canvas, and its band-pass levels `L_k = G_k - EXPAND(G_{k+1})`
//!    (`levels.rs`). Samples that aren't finite count as uncovered; samples are clamped to [`crate::MAX_SAMPLE`].
//! 2. **Salience**: the local energy of each band's luminance (Rec. 709 of the coefficients, which is the band of the
//!    luminance), its squares over a 5x5 binomial window: P. J. Burt and R. J. Kolczynski's salience ("Enhanced image
//!    capture through fusion", ICCV 1993). With [`Decisions::Channels`], each channel's own energy instead.
//! 3. **Selection**: at every level but the finest, each position keeps the coefficients of the frame with the most
//!    energy so far (its energy weighed by the fourth power of its pooled coverage, so that a frame that covers only
//!    part of a coefficient's support loses to one that covers all of it), and that energy: a running best-so-far
//!    pyramid and energy map. Ties keep the earlier frame.
//! 4. **Grit suppression** at the finest level: picking the largest of several frames' noise there builds up grit, so
//!    the finest level is a soft blend, each frame's coefficients weighted by its energy (scaled as the weighted
//!    average scales it) raised to `1 / grit`: where one frame is clearly sharpest it wins, where the frames' energies
//!    are alike (noise, flat regions) they average and their noise with them. `grit` 0 picks, as the other levels do.
//!    Measured on noisy synthetic stacks, the blend has less than half the noise of picking, and less than the weighted
//!    average's.
//! 5. **Residual**: the coarsest level is the average of the frames', weighted as choices are by their pooled
//!    coverage, which keeps the stack's brightness that of its frames.
//! 6. **Range**: each pixel's least and largest value over the frames that cover it are kept per channel
//!    (`range.rs`), and the result is held within them ([`PyramidSettings::range_limit`]): no pixel comes out darker or
//!    brighter than every frame, which removes the inversion halos and color casts of selection.
//!
//! The result is collapsed from the coarsest level down, and is an unclipped float image: values beyond `[0, 1]` stay
//! (they can only come from frames that have them, with the range limit on). [`compress_range`] brings it into range
//! as [`PyramidSettings::tone`] says; the engine keeps the unclipped result for "retain extended range" output.
//!
//! # Levels and tiles
//!
//! The number of levels is [`PyramidSettings::levels`], or by default as many as leave the coarsest level about 16
//! samples on its short side (`log2(short side) - 4`, from 1 to [`MAX_LEVELS`]): 5 for 720x540 frames, 7 for 20 MP.
//! The finest [`TILED_LEVELS`] levels are computed in tiles, with the reach their filters have
//! ([`Pyramid::context`]); the coarser levels and the residual on the whole canvas at `1 / 2^TILED_LEVELS` of its
//! size, from each tile's share of the frame's Gaussian level there. Tiles read the canvas from a multiple of
//! `2^TILED_LEVELS` pixels, so every tile's levels lie on the canvas's lattice, and the filters have fixed taps: the
//! result is the same bit for bit on one tile or many, one thread or many.
//!
//! # The coarse limit
//!
//! [`PyramidSettings::coarse_limit`], off by default, limits how far the coarse levels' choices may stray from the
//! depth-consistent frame: at levels from [`TILED_LEVELS`] up, a coefficient is kept only if its frame is at most that
//! many frames (in the order they were added, which is focus order) from the frame whose finest-level energy, pooled
//! to that level, is largest there; otherwise the depth-consistent frame's coefficient is used. Measured on synthetic
//! stacks whose layers occlude one another, it made halos slightly worse, not better (at an occluding edge the frame
//! in focus on the background is the one its out-of-focus foreground veils), and the range limit is what stops
//! inversion halos; it is there for scenes it suits, and for the depth map of roadmap 3.3 to replace its estimate.

mod kernel;
pub mod levels;
mod range;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use kernel::Pyramid;
pub use range::{BLOCK, Tone, compress_range, out_of_range};

use crate::error::StackError;
use crate::method::{Method, Stacker};
use crate::tiled::{Tiled, Tiling};

/// The method's name, as outputs and commands record it.
pub const PYRAMID: &str = "pyramid";
/// The most levels a pyramid may have.
pub const MAX_LEVELS: u32 = 10;
/// The finest levels, computed in tiles; the coarser ones are computed on the whole canvas.
pub const TILED_LEVELS: usize = 3;
/// The range of a `grit` that blends (0 picks).
pub const GRIT_RANGE: (f64, f64) = (0.25, 1.0);
/// The largest `coarse_limit`, in frames.
pub const MAX_COARSE_LIMIT: u32 = 1000;

/// What the pyramid's choices are made on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Decisions {
    /// One choice per position for all channels, on luminance: a pixel's channels come from the same frames, so its
    /// color is one some frame has.
    #[default]
    Luminance,
    /// Each channel chosen on its own energy: chromatic detail from whichever frame has it sharpest, at the risk of
    /// colored fringes where the lens's chromatic aberration makes the channels sharp in different frames.
    Channels,
}

/// The pyramid method's settings. See the module documentation. Every field has a default, so that a preset can name
/// only some.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct PyramidSettings {
    /// The number of pyramid levels, from 1 to 10; 0 chooses from the canvas's size (about 16 samples on the short
    /// side of the coarsest level: 5 for 720x540, 7 for 20 MP). More levels decide coarser structure by selection
    /// rather than by averaging the frames.
    #[schemars(range(max = 10))]
    pub levels: u32,
    /// Grit suppression at the finest level, from 0.25 (closest to picking) to 1 (blends most), or 0 (off: the
    /// sharpest frame's coefficient is picked, as at the other levels, which keeps the most fine detail and builds up
    /// the most noise). The finest level's coefficients are blended with weights of their energy raised to `1 / grit`.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub grit: f64,
    /// Whether choices are made on luminance (one choice for all channels, so colors stay together) or on each
    /// channel.
    pub decisions: Decisions,
    /// How strictly each pixel is held within the range the frames span there, from 0 (off) to 1 (never darker or
    /// brighter than every frame): removes the inversion halos and color casts of selection.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub range_limit: f64,
    /// How the result is brought into `[0, 1]`: `compress` (a local shoulder for highlights, negatives to 0), `clip`
    /// or `keep` (values outside stay, for those who tone-map themselves).
    pub tone: Tone,
    /// Off by default: the most frames the coarse levels' choices may be from the depth-consistent frame (the one
    /// whose finest-level energy is largest there), from 0 to 1000.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(max = 1000))]
    pub coarse_limit: Option<u32>,
}

impl Default for PyramidSettings {
    fn default() -> Self {
        PyramidSettings { levels: 0, grit: 0.5, decisions: Decisions::Luminance, range_limit: 1.0, tone: Tone::Compress, coarse_limit: None }
    }
}

impl PyramidSettings {
    /// Checks every setting's range.
    pub fn validate(&self) -> Result<(), StackError> {
        if self.levels > MAX_LEVELS {
            return Err(StackError::Setting { name: "levels", value: f64::from(self.levels), expected: "from 0 (automatic) to 10" });
        }
        #[allow(clippy::float_cmp)] // Exactly 0 is the documented "off".
        let grit_ok = self.grit == 0.0 || (self.grit.is_finite() && (GRIT_RANGE.0..=GRIT_RANGE.1).contains(&self.grit));
        if !grit_ok {
            return Err(StackError::Setting { name: "grit", value: self.grit, expected: "0 (off) or from 0.25 to 1" });
        }
        if !(self.range_limit.is_finite() && (0.0..=1.0).contains(&self.range_limit)) {
            return Err(StackError::Setting { name: "range_limit", value: self.range_limit, expected: "from 0 to 1" });
        }
        if let Some(d) = self.coarse_limit
            && d > MAX_COARSE_LIMIT
        {
            return Err(StackError::Setting { name: "coarse_limit", value: f64::from(d), expected: "from 0 to 1000 frames" });
        }
        Ok(())
    }

    /// The number of levels a `width` x `height` canvas gets: the setting, or the automatic choice, and never more than
    /// halving the short side down to one sample allows.
    pub fn levels_for(&self, width: usize, height: usize) -> usize {
        let short = width.min(height).max(1);
        let most = (short.ilog2() as usize).max(1);
        let chosen = if self.levels == 0 { (short.ilog2() as usize).saturating_sub(4) } else { self.levels as usize };
        chosen.clamp(1, MAX_LEVELS as usize).min(most)
    }
}

impl Method for PyramidSettings {
    fn name(&self) -> &'static str {
        PYRAMID
    }

    fn validate(&self) -> Result<(), StackError> {
        PyramidSettings::validate(self)
    }

    fn stacker(&self, tiling: &Tiling) -> Result<Box<dyn Stacker>, StackError> {
        Ok(Box::new(Tiled::new(Pyramid::new(self)?, tiling.clone())))
    }

    /// An explicit number of levels loses one per halving of the frames' size (the automatic choice follows the
    /// reduced canvas by itself); the other settings have no length.
    fn reduced(&self, factor: f64) -> Self {
        let factor = if factor.is_finite() && factor > 1.0 { factor } else { 1.0 };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // log2 of a factor above 1, rounded, small.
        let fewer = factor.log2().round() as u32;
        let levels = if self.levels == 0 { 0 } else { self.levels.saturating_sub(fewer).max(1) };
        PyramidSettings { levels, ..self.clone() }
    }
}
