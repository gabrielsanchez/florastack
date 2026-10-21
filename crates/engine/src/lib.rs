//! The engine: the command registry through which every user-facing action runs, the job system for long, cancellable
//! work, and the orchestration that ties the other crates together. It may depend on every FloraStack library crate
//! except `florastack-metrics`, and it re-exports the types the apps need, so that the apps depend on it alone.
//!
//! - [`frames`]: reading a stack's frames, each file sent to the crate that reads it (camera RAW and DNG files to
//!   florastack-raw, developed alike; 16-bit TIFF and other image files to florastack-codecs, unchanged).

pub mod frames;

pub use florastack_core::branding;
// What reading frames takes and gives, for the apps (the command registry of roadmap 1.5 will add the rest).
pub use florastack_codecs::{OrientationPolicy, ReadOptions};
pub use florastack_raw::{ChromaticAberration, Demosaic, DevelopSettings, Highlights, NoiseReduction, Quality, RawDevelop, Resolved, WhiteBalance};
