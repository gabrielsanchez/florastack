//! Frame alignment: a per-frame shift, scale, rotation and brightness gain relative to a master frame, stack ordering and
//! output framing, and the defect handling (dust, hot pixels, subject movement) that has to run in sensor coordinates
//! before frames are warped. Its CPU code is the reference that the GPU kernels are tested against. It may depend only on
//! `florastack-core`.
//!
//! # Aligning a stack
//!
//! [`StackAligner`] (or [`align_frames`], for frames already in memory) estimates, for every frame, the
//! [`Similarity`] that maps the master frame's pixels to the same points of the scene in the frame, plus the gain that
//! matches its brightness to the master's. Each term can be turned off in [`AlignSettings`], which makes alignment
//! faster and more accurate when the stack doesn't need it.
//!
//! 1. **Preparation**, once per frame: the frame's linear luminance, reduced to a working resolution (2048 pixels on
//!    its longer side by default), and a Gaussian pyramid of its band-pass, a difference of Gaussians that keeps edges
//!    and texture and, being linear, doesn't move them when the focus changes their blur.
//! 2. **First guess**: phase correlation of the two frames on a coarse level gives the shift; scale and rotation start
//!    from the previous link's, since focus breathing changes smoothly.
//! 3. **Refinement**, coarse to fine: the enhanced correlation coefficient (ECC) of the band-passed frames is
//!    maximized on every level of the pyramid, from the coarsest to the working resolution, to a fraction of a pixel,
//!    with a defocus term and robust weights that keep a change of focus from passing for motion.
//! 4. **Chaining**: each frame is aligned to its neighbor, which is focused at nearly the same depth, and the links
//!    are composed back to the master; optionally each frame is then refined directly against the master.
//! 5. **Brightness**: the median ratio of the master's to the frame's strongly low-passed luminance, which defocus
//!    barely changes, since a blur keeps local averages.
//!
//! Each frame's [`FrameAlignment`] says how well it matched (its score) and carries [`Flag`]s for what needs a look:
//! a term at its limit, a low score, no match, or an ambiguous match on a repeating pattern. Frames that hit a limit,
//! score low or don't match are rejected unless [`AlignSettings::reject`] is off, and the chain goes around them.
//! The modules document each step and the papers it follows.
//!
//! Everything is deterministic: rows are processed in parallel, but every sum is added up in row order, so results
//! are the same bit for bit on every run and thread count. Frames that can't be aligned are flagged, not errors;
//! [`AlignError`] is for what makes the stack itself impossible to align (frames of different sizes, settings out of
//! range, cancellation).

mod brightness;
mod ecc;
mod error;
mod fft;
mod model;
mod phase;
mod prepare;
mod settings;
mod stack;

pub use error::AlignError;
pub use model::Similarity;
pub use prepare::MIN_SIDE;
pub use settings::{AlignSettings, MAX_RESOLUTION, MIN_RESOLUTION, Term};
pub use stack::{Flag, FrameAlignment, StackAligner, StackAlignment, Status, align_frames};
