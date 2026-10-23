//! Frame alignment: a per-frame shift, scale, rotation and brightness gain relative to a master frame, stack ordering and
//! output framing, the defect handling (dust, hot pixels) that has to run in sensor coordinates before frames are
//! warped, subject movement and the filmstrip's previews. Its CPU code is the reference that the GPU kernels are tested
//! against. It may depend only on `florastack-core`.
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
//!
//! # Ordering, framing and focus
//!
//! - **Out-of-focus frames** ([`Flag::OutOfFocus`]): while aligning, each frame's focus map is kept, and once the
//!   stack is aligned, each frame's sharp-content fraction ([`FrameAlignment::sharp_fraction`]) says on how much of
//!   the stack's in-focus detail it is among the sharpest. Frames with next to none
//!   ([`AlignSettings::min_sharp_fraction`]), the ones focused in front of the subject or behind it, are flagged for
//!   the user to exclude (`focus.rs`).
//! - **Processing order** ([`ProcessingOrder`], [`narrow_end`]): from the end whose frame shows the scene larger, the
//!   narrow end, so that every frame covers the output; overridable with [`AlignSettings::automatic_order`] and
//!   [`AlignSettings::reverse_order`]. [`order_stack`] rebases the alignment onto that end's frame
//!   ([`StackAlignment::rebased`]), and [`align_ordered`] aligns frames in memory that way (`order.rs`).
//! - **Framing** ([`Framing`], [`Canvas`]): the output's pixel grid, cropped to the narrow end's field of view, keeping
//!   the widest one, or cropped to what every frame covers (`framing.rs`). Each frame's [`Coverage`] of the canvas,
//!   which follows from its transform, marks the pixels it has nothing for, so that stacking leaves the frame out
//!   there instead of smearing its edge pixels into streaks; [`warp_to_canvas`] warps a frame onto a tile of the
//!   canvas with its coverage (`coverage.rs`).
//! - **Sorting by scale** ([`scale_order`], [`align_by_scale`]): a first pass recovers each frame's scale, the frames
//!   are put in scale order, which is focus order, and aligned again (`sort.rs`).
//!
//! # Defects, movement and the filmstrip
//!
//! - **Sensor defects** ([`DefectMask`], `defects.rs`): dust and hot pixels sit at the same sensor pixels in every
//!   frame, so they are repaired there, before alignment warps the frames into trails of copies. A mask comes from the
//!   user (an image of black spots on white, [`DefectMask::from_image`]) or from the frames ([`DefectDetector`],
//!   [`detect_defects`]: darker than all around by the same factor in every frame, or brighter than the neighbors in
//!   most), can be saved as such an image ([`DefectMask::to_image`]), and is filled in each frame from the pixels
//!   around it ([`inpaint`]).
//! - **Movement** ([`Motion`], [`MotionMap`], `motion.rs`): each frame's residual motion against the neighbor it was
//!   aligned to, cell by cell, by Lucas–Kanade with gain and defocus terms; the regions that moved where the frame is
//!   in focus, a score, and [`Flag::Moved`].
//! - **The filmstrip** ([`thumbnail`], [`aligned_thumbnail`], `filmstrip.rs`): small previews of each frame, as shot
//!   and warped onto the output's canvas, for scrubbing through the stack.

mod brightness;
mod coverage;
mod defects;
mod ecc;
mod error;
mod fft;
mod filmstrip;
mod focus;
mod framing;
mod model;
mod motion;
mod order;
mod phase;
mod prepare;
mod settings;
mod sort;
mod stack;

pub use coverage::{Coverage, Covered, DEFAULT_FEATHER, coverage_count, warp_to_canvas};
pub use defects::{
    DefectDetector, DefectMask, MAX_DETECTION_FRAMES, MAX_SPOTS, MIN_DETECTION_FRAMES, Spot, detect_defects, detection_frames, inpaint,
};
pub use error::AlignError;
pub use filmstrip::{MIN_THUMBNAIL_SIDE, aligned_thumbnail, thumbnail};
pub use framing::{Canvas, Framing, MAX_FACTOR};
pub use model::Similarity;
pub use motion::{Motion, MotionCell, MotionMap, MotionRegion};
pub use order::{End, NARROW_MIN, ProcessingOrder, align_ordered, narrow_end, order_stack};
pub use prepare::MIN_SIDE;
pub use settings::{AlignSettings, DEFAULT_MIN_MOTION, DEFAULT_MIN_SHARP_FRACTION, MAX_RESOLUTION, MIN_RESOLUTION, Term};
pub use sort::{MIN_SPREAD, align_by_scale, measuring_settings, scale_order};
pub use stack::{Flag, FrameAlignment, StackAligner, StackAlignment, Status, align_frames};
